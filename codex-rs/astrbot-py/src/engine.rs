//! Rust-side engine: owns one ThreadManager and the threads AstrBot drives.
//!
//! Everything crossing the Python boundary is JSON so the binding stays thin
//! and does not have to mirror codex types.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_code_mode::ProcessOwnedCodeModeSessionProvider;
use codex_core::CodexAppsToolsCache;
use codex_core::CodexThread;
use codex_core::StartThreadOptions;
use codex_core::ThreadManager;
use codex_core::TurnInputSubmission;
use codex_core::build_models_manager;
use codex_core::config::Config;
use codex_core::config::ConfigBuilder;
use codex_core::config::ConfigOverrides;
use codex_core::init_state_db;
use codex_core::local_agent_graph_store_from_state_db;
use codex_core::passthrough_image_store;
use codex_core::resolve_installation_id;
use codex_core::thread_store_from_config;
use codex_exec_server::EnvironmentManager;
use codex_exec_server::ExecServerRuntimePaths;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_home::CodexHomeUserInstructionsProvider;
use codex_image_generation_extension::SavedImage;
use codex_image_generation_extension::SavedImageHook;
use codex_login::AuthManager;
use codex_protocol::ThreadId;
use codex_protocol::dynamic_tools::DynamicToolResponse;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AdditionalContextEntry;
use codex_protocol::protocol::AdditionalContextKind;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::user_input::UserInput;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use serde_json::json;
use tokio::sync::RwLock;

use crate::account::Accounts;
use crate::convert::json_overrides_to_toml;

const ORIGINATOR: &str = "astrbot";

/// Process-wide options, fixed when the runtime is created.
#[derive(Debug, Default, Deserialize)]
pub struct EngineOptions {
    /// CODEX_HOME owned by AstrBot (auth, config.toml, rollouts, state db).
    pub codex_home: PathBuf,
    /// Dotted config overrides applied to every thread (`-c key=value`).
    #[serde(default)]
    pub config: serde_json::Map<String, JsonValue>,
    /// `codex` executable used for sandbox / fs helper re-exec. Optional:
    /// without it, the local exec environment is unavailable.
    #[serde(default)]
    pub codex_self_exe: Option<PathBuf>,
    /// `codex-code-mode-host` executable. Required for code mode, because
    /// the default lookup next to `current_exe()` finds python.exe.
    #[serde(default)]
    pub code_mode_host: Option<PathBuf>,
    /// Ask for approval before every native command (and patch), so the host
    /// can decide per sender. Applied as a harness override because config
    /// files no longer accept `approval_policy = "untrusted"`.
    #[serde(default)]
    pub approve_every_command: bool,
}

/// Per-thread parameters for start and resume.
#[derive(Debug, Default, Deserialize)]
pub struct ThreadParams {
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_provider: Option<String>,
    #[serde(default)]
    pub base_instructions: Option<String>,
    #[serde(default)]
    pub developer_instructions: Option<String>,
    /// Dotted config overrides for this thread only.
    #[serde(default)]
    pub config: serde_json::Map<String, JsonValue>,
    #[serde(default)]
    pub dynamic_tools: Vec<DynamicToolSpec>,
    #[serde(default)]
    pub ephemeral: bool,
    /// Give the thread no execution environment: drops shell, apply_patch
    /// and view_image. AstrBot supplies execution through its own tools.
    #[serde(default)]
    pub no_environment: bool,
    /// Rollout file to resume (resume only).
    #[serde(default)]
    pub rollout_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnMode {
    StartOrSteer,
    StartIfIdle,
    Steer,
}

#[derive(Debug, Deserialize)]
pub struct ContextEntry {
    pub value: String,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TurnRequest {
    pub input: Vec<UserInput>,
    #[serde(default = "default_turn_mode")]
    pub mode: TurnMode,
    #[serde(default)]
    pub expected_turn_id: Option<String>,
    /// Standing context, re-sent by core only when a value changes.
    #[serde(default)]
    pub additional_context: BTreeMap<String, ContextEntry>,
    /// Replace the thread's dynamic tools when this input is accepted.
    #[serde(default)]
    pub dynamic_tools: Option<Vec<DynamicToolSpec>>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<ReasoningEffort>,
}

fn default_turn_mode() -> TurnMode {
    TurnMode::StartOrSteer
}

#[derive(Debug, Deserialize)]
pub struct ReviewRequest {
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub turn_id: Option<String>,
    pub approved: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Host callback for saved images, filled in after the engine exists: the
/// extension is installed while the engine is built, before the host can hand
/// over a callback.
type SavedImageSlot = Arc<std::sync::RwLock<Option<SavedImageHook>>>;

pub struct Engine {
    options: EngineOptions,
    base_overrides: Vec<(String, toml::Value)>,
    thread_manager: Arc<ThreadManager>,
    threads: RwLock<HashMap<String, Arc<CodexThread>>>,
    base_config: Config,
    auth_manager: Arc<AuthManager>,
    accounts: Accounts,
    saved_image_hook: SavedImageSlot,
}

impl Engine {
    pub async fn new(mut options: EngineOptions) -> Result<Self> {
        if !options.codex_home.is_absolute() {
            options.codex_home = std::env::current_dir()?.join(&options.codex_home);
        }
        let _ = codex_login::default_client::set_default_originator(ORIGINATOR.to_string());
        let base_overrides = json_overrides_to_toml(&options.config)?;
        let config = build_config(&options, &base_overrides, &ThreadParams::default()).await?;
        let state_db = init_state_db(&config).await;
        let auth_manager =
            AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false)
                .await
                .map_err(|err| anyhow!("auth manager: {err:?}"))?;
        // Without a codex executable there is no local exec environment; AstrBot
        // then provides execution through its own tools (the default).
        let environment_manager = Arc::new(match options.codex_self_exe.clone() {
            Some(exe) => EnvironmentManager::from_codex_home(
                config.codex_home.clone(),
                Some(ExecServerRuntimePaths::from_optional_paths(
                    Some(exe),
                    None,
                )?),
                config.http_client_factory(),
            )
            .await
            .map_err(|err| anyhow!("environment manager: {err}"))?,
            None => EnvironmentManager::without_environments(config.http_client_factory()),
        });
        let thread_store = thread_store_from_config(&config, state_db.clone());
        let installation_id = resolve_installation_id(&config.codex_home).await?;
        let user_instructions_provider = Arc::new(CodexHomeUserInstructionsProvider::new(
            config.codex_home.clone(),
        ));
        let saved_image_hook = SavedImageSlot::default();
        let extensions = build_extensions(Arc::clone(&auth_manager), Arc::clone(&saved_image_hook));
        let base_config = config.clone();
        let mut thread_manager = ThreadManager::new(
            &config,
            Arc::clone(&auth_manager),
            build_models_manager(&config, Arc::clone(&auth_manager)),
            CodexAppsToolsCache::default(),
            SessionSource::Custom(ORIGINATOR.to_string()),
            environment_manager,
            Arc::new(extensions),
            user_instructions_provider,
            /*analytics_events_client*/ None,
            passthrough_image_store(),
            thread_store,
            local_agent_graph_store_from_state_db(state_db.as_ref()),
            installation_id,
            /*attestation_provider*/ None,
            /*external_time_provider*/ None,
        );
        if let Some(host) = options.code_mode_host.clone() {
            thread_manager = thread_manager.with_code_mode_session_provider(Arc::new(
                ProcessOwnedCodeModeSessionProvider::with_host_program(host),
            ));
        }
        Ok(Self {
            options,
            base_overrides,
            thread_manager: Arc::new(thread_manager),
            threads: RwLock::new(HashMap::new()),
            base_config,
            auth_manager,
            accounts: Accounts::default(),
            saved_image_hook,
        })
    }

    /// Runs the saved-image callback as Codex would. Exists so the host can
    /// check the round trip through the binding without generating an image.
    pub async fn fire_saved_image_hook(&self, image: SavedImage) -> Option<String> {
        let hook = match self.saved_image_hook.read() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }?;
        hook(image).await
    }

    /// Sets (or clears) the callback run after Codex saves a generated image.
    /// The text it returns becomes the tool result the model sees.
    pub fn set_saved_image_hook(&self, hook: Option<SavedImageHook>) {
        match self.saved_image_hook.write() {
            Ok(mut slot) => *slot = hook,
            Err(poisoned) => *poisoned.into_inner() = hook,
        }
    }

    async fn thread_config(&self, params: &ThreadParams) -> Result<Config> {
        let mut overrides = self.base_overrides.clone();
        overrides.extend(json_overrides_to_toml(&params.config)?);
        build_config(&self.options, &overrides, params).await
    }

    fn start_options(&self, config: Config, params: ThreadParams) -> StartThreadOptions {
        let mut options = StartThreadOptions::new(config);
        options.dynamic_tools = params.dynamic_tools;
        if params.no_environment {
            options.environments = Some(Vec::new());
        }
        options
    }

    pub async fn start_thread(&self, params: ThreadParams) -> Result<JsonValue> {
        let config = self.thread_config(&params).await?;
        let new_thread = self
            .thread_manager
            .start_thread(self.start_options(config, params))
            .await
            .context("start thread")?;
        self.register(new_thread.thread_id, &new_thread.thread)
            .await;
        Ok(thread_info(
            &new_thread.thread_id,
            &new_thread.thread,
            &new_thread.session_configured,
        ))
    }

    pub async fn resume_thread(&self, params: ThreadParams) -> Result<JsonValue> {
        let rollout_path = params
            .rollout_path
            .clone()
            .ok_or_else(|| anyhow!("resume requires rollout_path"))?;
        let config = self.thread_config(&params).await?;
        // Resume through start_thread so `no_environment` applies like on start;
        // dynamic tools come from the rollout unless the caller sends new ones.
        let history = codex_rollout::RolloutRecorder::get_rollout_history(&rollout_path)
            .await
            .with_context(|| format!("read rollout {}", rollout_path.display()))?;
        let session_source = history
            .get_resumed_session_sources()
            .map(|(source, _)| source);
        let mut options = self.start_options(config, params);
        options.initial_history = history;
        options.session_source = session_source;
        let new_thread = self
            .thread_manager
            .start_thread(options)
            .await
            .context("resume thread")?;
        self.register(new_thread.thread_id, &new_thread.thread)
            .await;
        Ok(thread_info(
            &new_thread.thread_id,
            &new_thread.thread,
            &new_thread.session_configured,
        ))
    }

    async fn register(&self, thread_id: ThreadId, thread: &Arc<CodexThread>) {
        self.threads
            .write()
            .await
            .insert(thread_id.to_string(), Arc::clone(thread));
    }

    pub async fn thread(&self, thread_id: &str) -> Result<Arc<CodexThread>> {
        self.threads
            .read()
            .await
            .get(thread_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown thread {thread_id}"))
    }

    pub async fn is_loaded(&self, thread_id: &str) -> bool {
        self.threads.read().await.contains_key(thread_id)
    }

    pub async fn submit_turn(&self, thread_id: &str, request: TurnRequest) -> Result<JsonValue> {
        let thread = self.thread(thread_id).await?;
        let mut turn = TurnInputRequest::user_input(request.input);
        turn.additional_context = request
            .additional_context
            .into_iter()
            .map(|(key, entry)| {
                let kind = match entry.kind.as_deref() {
                    Some("untrusted") => AdditionalContextKind::Untrusted,
                    _ => AdditionalContextKind::Application,
                };
                (
                    key,
                    AdditionalContextEntry {
                        value: entry.value,
                        kind,
                    },
                )
            })
            .collect();
        turn.thread_settings = ThreadSettingsOverrides {
            dynamic_tools: request.dynamic_tools,
            model: request.model,
            effort: request.effort.map(Some),
            ..Default::default()
        };
        let submission = match request.mode {
            TurnMode::StartOrSteer => thread.start_or_steer_turn(turn).await?,
            TurnMode::StartIfIdle => match thread.start_turn_if_idle(turn).await? {
                codex_core::StartIfIdleSubmission::Started { turn_id } => {
                    TurnInputSubmission::Started { turn_id }
                }
                codex_core::StartIfIdleSubmission::NotSubmitted { reason } => {
                    TurnInputSubmission::NotSubmitted { reason }
                }
            },
            TurnMode::Steer => {
                let expected = request
                    .expected_turn_id
                    .ok_or_else(|| anyhow!("steer requires expected_turn_id"))?;
                match thread.steer_turn(turn, expected).await? {
                    codex_core::SteerSubmission::Steered { turn_id } => {
                        TurnInputSubmission::Steered { turn_id }
                    }
                    codex_core::SteerSubmission::NotSubmitted { reason } => {
                        TurnInputSubmission::NotSubmitted { reason }
                    }
                }
            }
        };
        if matches!(submission, TurnInputSubmission::Started { .. }) {
            self.start_memories(thread_id, &thread).await;
        }
        Ok(match submission {
            TurnInputSubmission::Started { turn_id } => {
                json!({"status": "started", "turn_id": turn_id})
            }
            TurnInputSubmission::Steered { turn_id } => {
                json!({"status": "steered", "turn_id": turn_id})
            }
            TurnInputSubmission::NotSubmitted { reason } => {
                json!({"status": "not_submitted", "reason": format!("{reason:?}")})
            }
        })
    }

    /// Next event, or `None` once the thread has terminated and its queue is drained.
    pub async fn next_event(thread: Arc<CodexThread>) -> Result<Option<JsonValue>> {
        let event = tokio::select! {
            biased;
            event = thread.next_event() => event?,
            () = thread.wait_until_terminated() => {
                if thread.queued_event_count() == 0 {
                    return Ok(None);
                }
                thread.next_event().await?
            }
        };
        Ok(Some(serde_json::to_value(&event)?))
    }

    pub async fn dynamic_tool_response(
        &self,
        thread_id: &str,
        call_id: String,
        response: DynamicToolResponse,
    ) -> Result<()> {
        let thread = self.thread(thread_id).await?;
        thread
            .submit(Op::DynamicToolResponse {
                id: call_id,
                response,
            })
            .await?;
        Ok(())
    }

    pub async fn set_dynamic_tools(
        &self,
        thread_id: &str,
        tools: Vec<DynamicToolSpec>,
    ) -> Result<()> {
        let thread = self.thread(thread_id).await?;
        thread
            .submit(Op::ThreadSettings {
                thread_settings: ThreadSettingsOverrides {
                    dynamic_tools: Some(tools),
                    ..Default::default()
                },
            })
            .await?;
        Ok(())
    }

    /// Background memory extraction / consolidation after a turn starts, like
    /// the app server. A no-op unless `features.memories` is on for the thread.
    async fn start_memories(&self, thread_id: &str, thread: &Arc<CodexThread>) {
        let Ok(id) = ThreadId::from_string(thread_id) else {
            return;
        };
        let snapshot = thread.config_snapshot().await;
        codex_memories_write::start_memories_startup_task(
            Arc::clone(&self.thread_manager),
            Arc::clone(&self.auth_manager),
            id,
            Arc::clone(thread),
            thread.config().await,
            snapshot.permission_profile,
            &snapshot.session_source,
        );
    }

    /// Run memory extraction and consolidation now for this thread's
    /// partitions (global, then its scope), ignoring `auto_consolidate`.
    pub async fn consolidate_memories(&self, thread_id: &str, force: bool) -> Result<()> {
        let thread = self.thread(thread_id).await?;
        let id = ThreadId::from_string(thread_id)?;
        let snapshot = thread.config_snapshot().await;
        codex_memories_write::run_memories_consolidation_now(
            Arc::clone(&self.thread_manager),
            Arc::clone(&self.auth_manager),
            id,
            Arc::clone(&thread),
            thread.config().await,
            snapshot.permission_profile,
            &snapshot.session_source,
            force,
        )
        .await
    }

    /// Answer an `exec_approval_request` (`kind = "exec"`) or
    /// `apply_patch_approval_request` (`kind = "patch"`).
    pub async fn review_decision(&self, thread_id: &str, request: ReviewRequest) -> Result<()> {
        let thread = self.thread(thread_id).await?;
        let decision = if request.approved {
            ReviewDecision::Approved
        } else {
            ReviewDecision::denied(request.reason.unwrap_or_else(|| "denied".to_string()))
        };
        let op = match request.kind.as_str() {
            "exec" => Op::ExecApproval {
                id: request.id,
                turn_id: request.turn_id,
                decision,
            },
            "patch" => Op::PatchApproval {
                id: request.id,
                decision,
            },
            other => return Err(anyhow!("unknown approval kind: {other}")),
        };
        thread.submit(op).await?;
        Ok(())
    }

    pub async fn interrupt(&self, thread_id: &str) -> Result<()> {
        let thread = self.thread(thread_id).await?;
        thread.submit(Op::Interrupt).await?;
        Ok(())
    }

    pub async fn shutdown_thread(&self, thread_id: &str) -> Result<()> {
        let Some(thread) = self.threads.write().await.remove(thread_id) else {
            return Ok(());
        };
        let result = thread.shutdown_and_wait().await;
        if let Ok(id) = ThreadId::from_string(thread_id) {
            let _ = self
                .thread_manager
                .remove_thread_if_matches(&id, &thread)
                .await;
        }
        result.map_err(Into::into)
    }

    pub async fn account_status(&self) -> JsonValue {
        Accounts::status(&self.auth_manager).await
    }

    pub async fn login_api_key(&self, api_key: &str) -> Result<()> {
        Accounts::login_api_key(&self.base_config, &self.auth_manager, api_key).await
    }

    pub async fn start_device_login(&self) -> Result<JsonValue> {
        self.accounts
            .start_device_login(&self.base_config, Arc::clone(&self.auth_manager))
            .await
    }

    pub async fn device_login_status(&self, login_id: &str) -> JsonValue {
        self.accounts.device_login_status(login_id).await
    }

    pub async fn cancel_device_login(&self, login_id: &str) -> bool {
        self.accounts.cancel_device_login(login_id).await
    }

    pub async fn logout(&self) -> Result<bool> {
        Accounts::logout(&self.auth_manager).await
    }

    pub async fn list_models(&self, include_hidden: bool) -> JsonValue {
        Accounts::list_models(&self.base_config, &self.thread_manager, include_hidden).await
    }

    pub async fn shutdown(&self) -> Result<()> {
        let ids: Vec<String> = self.threads.read().await.keys().cloned().collect();
        for id in ids {
            let _ = self.shutdown_thread(&id).await;
        }
        // Also stops threads we never saw, e.g. sub-agents.
        let _ = self
            .thread_manager
            .shutdown_all_threads_bounded(std::time::Duration::from_secs(10))
            .await;
        Ok(())
    }
}

fn thread_info(
    thread_id: &ThreadId,
    thread: &CodexThread,
    session_configured: &codex_protocol::protocol::SessionConfiguredEvent,
) -> JsonValue {
    json!({
        "thread_id": thread_id.to_string(),
        "rollout_path": thread.rollout_path(),
        "model": session_configured.model,
    })
}

async fn build_config(
    options: &EngineOptions,
    cli_overrides: &[(String, toml::Value)],
    params: &ThreadParams,
) -> Result<Config> {
    // Never fall back to the host process cwd: it would pick up project
    // `.codex/` layers from wherever AstrBot happens to run.
    let cwd = params
        .cwd
        .clone()
        .unwrap_or_else(|| options.codex_home.clone());
    let harness = ConfigOverrides {
        cwd: Some(cwd),
        model: params.model.clone(),
        model_provider: params.model_provider.clone(),
        base_instructions: params.base_instructions.clone(),
        developer_instructions: params.developer_instructions.clone(),
        ephemeral: Some(params.ephemeral),
        codex_self_exe: options.codex_self_exe.clone(),
        approval_policy: options
            .approve_every_command
            .then_some(AskForApproval::UnlessTrusted),
        ..Default::default()
    };
    ConfigBuilder::default()
        .codex_home(options.codex_home.clone())
        .cli_overrides(cli_overrides.to_vec())
        .harness_overrides(harness)
        .build()
        .await
        .context("load codex config")
}

fn build_extensions(
    auth_manager: Arc<AuthManager>,
    saved_image_hook: SavedImageSlot,
) -> codex_extension_api::ExtensionRegistry<Config> {
    let mut builder = ExtensionRegistryBuilder::<Config>::new();
    codex_memories_extension::install(&mut builder, /*metrics_client*/ None);
    codex_web_search_extension::install(&mut builder, Arc::clone(&auth_manager));
    // Codex's own image generation, saved under CODEX_HOME. It registers a
    // tool only for an OpenAI-authenticated account on a paid plan with an
    // image-capable model, so it costs nothing otherwise.
    //
    // The host is called after each saved image: CODEX_HOME is on the host and
    // out of reach of the model's tools, so AstrBot copies the image into the
    // chat's workspace and says where in the same tool result.
    let forward: SavedImageHook = Arc::new(move |image| {
        let current = match saved_image_hook.read() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        Box::pin(async move {
            match current {
                Some(hook) => hook(image).await,
                None => None,
            }
        })
    });
    codex_image_generation_extension::install_with_saved_image_hook(
        &mut builder,
        auth_manager,
        |config: &Config| Some(config.codex_home.clone()),
        Some(forward),
    );
    codex_skills_extension::install(&mut builder, |config: &Config| {
        codex_skills_extension::SkillsExtensionConfig {
            include_instructions: config.include_skill_instructions,
            max_context_tokens: config.skill_max_context_tokens,
            bundled_skills_enabled: config.bundled_skills_enabled(),
            orchestrator_skills_enabled: config.orchestrator_skills_enabled,
            shadow_selection_enabled: false,
        }
    });
    builder.build()
}
