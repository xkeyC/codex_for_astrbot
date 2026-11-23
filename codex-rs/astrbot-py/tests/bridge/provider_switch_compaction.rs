//! AstrBot: after a switch to a smaller model Codex compacts the history with
//! the previous model first. That model belongs to the provider it ran on:
//! after a provider switch the new provider may not serve it (commandcode
//! answered 403 MODEL_NOT_IN_PLAN for the ChatGPT model), so the compaction
//! uses the current model instead.

use anyhow::Result;
use codex_login::CodexAuth;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

/// 500k context window in the bundled catalog.
const BIG_MODEL: &str = "gpt-6.1-terra";
/// 272k context window: past the history's 300k tokens.
const SMALL_MODEL: &str = "gpt-6-luna";

fn on(provider: &str, model: &str) -> TestCodexBuilder {
    let provider = provider.to_string();
    test_codex()
        .with_auth(CodexAuth::from_api_key("key"))
        .with_model(model)
        .with_config(move |config| {
            config.model_provider_id = provider.clone();
            config
                .model_providers
                .insert(provider, config.model_provider.clone());
        })
}

fn answer(id: &str, tokens: i64) -> String {
    sse(vec![
        ev_assistant_message(&format!("m-{id}"), "ok"),
        ev_completed_with_tokens(id, tokens),
    ])
}

/// The model of every request the server got after the first turn. With
/// `failed_attempt`, the first turn after the switch fails on its first
/// request (as one did on the server before this fix: it applied the new
/// settings but recorded no turn) and the thread is resumed once more.
async fn models_after_resume(provider_after: &str, failed_attempt: bool) -> Result<Vec<String>> {
    let server = responses::start_mock_server().await;
    // The first answer fills 300k tokens; every later request gets a small one.
    let calls = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(move |_: &wiremock::Request| {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if failed_attempt && n == 1 {
                return ResponseTemplate::new(400).set_body_json(serde_json::json!({
                    "error": {"message": "refused", "type": "invalid_request_error"}
                }));
            }
            let tokens = if n == 0 { 300_000 } else { 1_000 };
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(answer(&format!("r{n}"), tokens))
        })
        .mount(&server)
        .await;
    let test = on("first", BIG_MODEL).build_with_auto_env(&server).await?;
    test.submit_text_turn("A long conversation.").await?;
    test.codex.shutdown_and_wait().await?;
    let path = test.codex.rollout_path().expect("rollout path");
    let mut test = on(provider_after, SMALL_MODEL)
        .resume(&server, test.home.clone(), path.clone())
        .await?;
    if failed_attempt {
        let _ = test.submit_text_turn("Refused.").await;
        test.codex.shutdown_and_wait().await?;
        test = on(provider_after, SMALL_MODEL)
            .resume(&server, test.home.clone(), path)
            .await?;
    }
    test.submit_text_turn("After the switch.").await?;

    let requests = server.received_requests().await.unwrap_or_default();
    Ok(requests
        .iter()
        .skip(1)
        .filter_map(|request| serde_json::from_slice::<Value>(&request.body).ok())
        .filter_map(|body| body["model"].as_str().map(str::to_string))
        .collect())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_provider_compacts_with_its_own_model() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let models = models_after_resume("second", false).await?;
    assert!(!models.is_empty());
    assert_eq!(
        models.iter().filter(|model| *model == BIG_MODEL).count(),
        0,
        "{models:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_provider_still_compacts_with_the_previous_model() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let models = models_after_resume("first", false).await?;
    assert_eq!(
        models.first().map(String::as_str),
        Some(BIG_MODEL),
        "{models:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_attempt_on_the_new_provider_does_not_make_it_the_previous_ones() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let models = models_after_resume("second", true).await?;
    assert!(models.len() > 1, "{models:?}");
    assert_eq!(
        models.iter().filter(|model| *model == BIG_MODEL).count(),
        0,
        "{models:?}"
    );
    Ok(())
}
