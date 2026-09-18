//! Account and model management for the AstrBot WebUI (fork addition).
//!
//! Mirrors the app-server account processor: API-key login, ChatGPT
//! device-code login (polled by the caller), logout, status and model list.
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use anyhow::Result;
use anyhow::anyhow;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_login::AuthManager;
use codex_login::ServerOptions;
use codex_login::complete_device_code_login;
use codex_login::login_with_api_key;
use codex_login::oauth_client_id;
use codex_login::request_device_code;
use codex_models_manager::manager::RefreshStrategy;
use serde_json::Value as JsonValue;
use serde_json::json;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[derive(Clone, Debug)]
enum LoginState {
    Pending,
    Success,
    Failed(String),
}

struct DeviceLogin {
    state: Arc<StdMutex<LoginState>>,
    task: JoinHandle<()>,
}

#[derive(Default)]
pub struct Accounts {
    logins: Mutex<HashMap<String, DeviceLogin>>,
}

impl Accounts {
    pub async fn status(auth_manager: &AuthManager) -> JsonValue {
        match auth_manager.auth().await {
            None => json!({ "logged_in": false }),
            Some(auth) => json!({
                "logged_in": true,
                "mode": auth.auth_mode(),
                "email": auth.get_account_email(),
                "account_id": auth.get_account_id(),
                "plan": auth.account_plan_type().map(|plan| format!("{plan:?}")),
            }),
        }
    }

    pub async fn login_api_key(
        config: &Config,
        auth_manager: &AuthManager,
        api_key: &str,
    ) -> Result<()> {
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(anyhow!("api key is empty"));
        }
        login_with_api_key(
            &config.codex_home,
            api_key,
            config.cli_auth_credentials_store_mode,
            config.auth_keyring_backend_kind(),
        )
        .map_err(|err| anyhow!("failed to save api key: {err}"))?;
        auth_manager.reload().await;
        Ok(())
    }

    /// Start a ChatGPT device-code login. Returns the code the user enters at
    /// the verification URL; completion is tracked under the returned login id.
    pub async fn start_device_login(
        &self,
        config: &Config,
        auth_manager: Arc<AuthManager>,
    ) -> Result<JsonValue> {
        let opts = ServerOptions {
            open_browser: false,
            ..ServerOptions::new(
                config.codex_home.to_path_buf(),
                oauth_client_id(),
                auth_manager.effective_chatgpt_workspaces(),
                config.cli_auth_credentials_store_mode,
                config.auth_keyring_backend_kind(),
                config.auth_route_config(),
            )
        };
        let device_code = request_device_code(&opts)
            .await
            .map_err(|err| anyhow!("device code request failed: {err}"))?;
        let verification_url = device_code.verification_url.clone();
        let user_code = device_code.user_code.clone();
        let login_id = uuid_like();
        let state = Arc::new(StdMutex::new(LoginState::Pending));
        let task_state = Arc::clone(&state);
        let task = tokio::spawn(async move {
            let result = complete_device_code_login(opts, device_code).await;
            let next = match result {
                Ok(()) => {
                    auth_manager.reload().await;
                    LoginState::Success
                }
                Err(err) => LoginState::Failed(err.to_string()),
            };
            if let Ok(mut guard) = task_state.lock() {
                *guard = next;
            }
        });
        let mut logins = self.logins.lock().await;
        // Only one login flow at a time, like the app server.
        for (_, old) in logins.drain() {
            old.task.abort();
        }
        logins.insert(login_id.clone(), DeviceLogin { state, task });
        Ok(json!({
            "login_id": login_id,
            "verification_url": verification_url,
            "user_code": user_code,
        }))
    }

    pub async fn device_login_status(&self, login_id: &str) -> JsonValue {
        let state = match self.logins.lock().await.get(login_id) {
            Some(login) => Arc::clone(&login.state),
            None => return json!({ "status": "unknown" }),
        };
        let current = state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| LoginState::Failed("login state poisoned".to_string()));
        match current {
            LoginState::Pending => json!({ "status": "pending" }),
            LoginState::Success => json!({ "status": "success" }),
            LoginState::Failed(error) => json!({ "status": "failed", "error": error }),
        }
    }

    pub async fn cancel_device_login(&self, login_id: &str) -> bool {
        match self.logins.lock().await.remove(login_id) {
            Some(login) => {
                login.task.abort();
                true
            }
            None => false,
        }
    }

    pub async fn logout(auth_manager: &AuthManager) -> Result<bool> {
        auth_manager
            .logout()
            .await
            .map_err(|err| anyhow!("logout failed: {err}"))
    }

    pub async fn list_models(
        config: &Config,
        thread_manager: &ThreadManager,
        include_hidden: bool,
    ) -> JsonValue {
        let presets = thread_manager
            .list_models(
                RefreshStrategy::OnlineIfUncached,
                config.http_client_factory(),
            )
            .await;
        let models: Vec<JsonValue> = presets
            .into_iter()
            .filter(|preset| include_hidden || preset.show_in_picker)
            .map(|preset| {
                json!({
                    "id": preset.id,
                    "model": preset.model,
                    "display_name": preset.display_name,
                    "description": preset.description,
                    "is_default": preset.is_default,
                    "default_reasoning_effort": preset.default_reasoning_effort,
                    "reasoning_efforts": preset
                        .supported_reasoning_efforts
                        .iter()
                        .map(|effort| effort.effort.clone())
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        JsonValue::Array(models)
    }
}

fn uuid_like() -> String {
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("login-{nanos:x}")
}
