//! AstrBot: encrypted history belongs to the provider and account that
//! produced it. Requests send the current identity's items back unchanged and
//! leave out the others', with a one-time notice per switch.

use anyhow::Result;
use codex_config::model_provider_options::ModelProviderOptions;
use codex_history::RolloutItem;
use codex_history::RolloutLine;
use codex_login::CodexAuth;
use codex_protocol::ThreadId;
use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use core_test_support::responses;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use wiremock::MockServer;

const NOTICE_TAG: &str = "<model_switch_notice>";
const DEFAULT_NOTICE: &str = "Note: the model provider or account of this conversation \
changed. Part of the earlier history was produced under the previous one in an encrypted form \
that cannot be read now, so some earlier context and summaries may be missing. If you need \
earlier context, look it up in the message history.";
const PLACEHOLDER: &str = "[encrypted content from a previous model omitted]";

/// A thread on provider id `provider`, signed in with API key `key`.
fn on(provider: &str, key: &str, notice: Option<&str>) -> TestCodexBuilder {
    let provider = provider.to_string();
    let notice = notice.map(str::to_string);
    test_codex()
        .with_auth(CodexAuth::from_api_key(key))
        .with_config(move |config| {
            config.model_provider_id = provider.clone();
            config
                .model_providers
                .insert(provider.clone(), config.model_provider.clone());
            config.model_provider_options.insert(
                provider,
                ModelProviderOptions {
                    open_model_switch_notice: notice,
                    ..Default::default()
                },
            );
        })
}

/// A response with reasoning carrying `token` as its encrypted content (as
/// DeepSeek's Responses API returns it), then an answer.
fn reasoning_response(id: &str, token: &str) -> String {
    sse(vec![
        responses::ev_response_created(id),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "reasoning",
                "id": format!("rs-{id}"),
                "summary": [],
                "content": [],
                "encrypted_content": token,
            }
        }),
        responses::ev_assistant_message(&format!("msg-{id}"), "Done."),
        responses::ev_completed(id),
    ])
}

/// The encrypted contents of the request's reasoning items.
fn reasoning_tokens(request: &ResponsesRequest) -> Vec<String> {
    request
        .inputs_of_type("reasoning")
        .iter()
        .filter_map(|item| item["encrypted_content"].as_str().map(str::to_string))
        .collect()
}

/// The notices of the request, in order.
fn notices(request: &ResponsesRequest) -> Vec<String> {
    request
        .message_input_texts("user")
        .into_iter()
        .filter(|text| text.starts_with(NOTICE_TAG))
        .collect()
}

fn notice(text: &str) -> String {
    format!("{NOTICE_TAG}\n{text}\n</model_switch_notice>")
}

/// Shuts the thread down and resumes it with `builder`.
async fn resume(
    server: &MockServer,
    test: TestCodex,
    mut builder: TestCodexBuilder,
) -> Result<TestCodex> {
    test.codex.shutdown_and_wait().await?;
    let path = test.codex.rollout_path().expect("rollout path");
    builder.resume(server, test.home.clone(), path).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn own_encrypted_reasoning_goes_back_unchanged_also_after_resume() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            reasoning_response("resp-1", "3a94aa8f-0001-0"),
            responses::sse_completed("resp-2"),
            responses::sse_completed("resp-3"),
        ],
    )
    .await;
    let test = on("deepseek", "key-ds", None)
        .build_with_auto_env(&server)
        .await?;
    test.submit_text_turn("Think.").await?;
    test.submit_text_turn("Again.").await?;
    let test = resume(&server, test, on("deepseek", "key-ds", None)).await?;
    test.submit_text_turn("After resume.").await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests[1..] {
        assert_eq!(reasoning_tokens(request), vec!["3a94aa8f-0001-0"]);
    }
    for request in &requests {
        assert_eq!(notices(request), Vec::<String>::new());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_provider_switch_leaves_the_old_items_out_with_one_notice() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            reasoning_response("resp-1", "gAAAA-openai"),
            reasoning_response("resp-2", "3a94aa8f-0002-0"),
            responses::sse_completed("resp-3"),
        ],
    )
    .await;
    let test = on("openai", "key-oa", None)
        .build_with_auto_env(&server)
        .await?;
    test.submit_text_turn("Think.").await?;
    let test = resume(&server, test, on("deepseek", "key-ds", None)).await?;
    test.submit_text_turn("Switched.").await?;
    test.submit_text_turn("Again.").await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    // OpenAI's reasoning is left out; DeepSeek's own goes back afterwards.
    assert_eq!(reasoning_tokens(&requests[1]), Vec::<String>::new());
    assert_eq!(reasoning_tokens(&requests[2]), vec!["3a94aa8f-0002-0"]);
    for request in &requests[1..] {
        assert!(!request.body_json().to_string().contains("gAAAA-openai"));
        assert_eq!(notices(request), vec![notice(DEFAULT_NOTICE)]);
    }
    // The notice comes just before the first message after the switch.
    let input = requests[1].input();
    let at = |text: &str| {
        input.iter().position(|item| {
            item["role"] == "user"
                && item["content"][0]["text"]
                    .as_str()
                    .is_some_and(|value| value.starts_with(text))
        })
    };
    assert_eq!(
        at("Switched.")
            .zip(at(NOTICE_TAG))
            .map(|(user, notice)| user - notice),
        Some(1)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_key_for_the_same_provider_is_a_switch() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            reasoning_response("resp-1", "3a94aa8f-key-a-0"),
            responses::sse_completed("resp-2"),
            responses::sse_completed("resp-3"),
        ],
    )
    .await;
    let test = on("deepseek", "key-a", None)
        .build_with_auto_env(&server)
        .await?;
    test.submit_text_turn("Think.").await?;
    let test = resume(
        &server,
        test,
        on("deepseek", "key-b", Some("Some history is gone.")),
    )
    .await?;
    test.submit_text_turn("New key.").await?;
    test.submit_text_turn("Again.").await?;

    let requests = mock.requests();
    for request in &requests[1..] {
        assert_eq!(reasoning_tokens(request), Vec::<String>::new());
        assert_eq!(notices(request), vec![notice("Some history is gone.")]);
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switching_back_leaves_out_the_intermediate_providers_items() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            reasoning_response("resp-1", "gAAAA-openai"),
            reasoning_response("resp-2", "3a94aa8f-0003-0"),
            responses::sse_completed("resp-3"),
            responses::sse_completed("resp-4"),
        ],
    )
    .await;
    let test = on("openai", "key-oa", None)
        .build_with_auto_env(&server)
        .await?;
    test.submit_text_turn("Think.").await?;
    let test = resume(&server, test, on("deepseek", "key-ds", None)).await?;
    test.submit_text_turn("Switched.").await?;
    let test = resume(&server, test, on("openai", "key-oa", None)).await?;
    test.submit_text_turn("Back.").await?;
    test.submit_text_turn("Again.").await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    // Back on OpenAI with the same key: its own reasoning is readable again
    // and goes back; DeepSeek's is left out.
    for request in &requests[2..] {
        assert_eq!(reasoning_tokens(request), vec!["gAAAA-openai"]);
        assert!(!request.body_json().to_string().contains("3a94aa8f-0003-0"));
        // One notice per switch.
        assert_eq!(
            notices(request),
            vec![notice(DEFAULT_NOTICE), notice(DEFAULT_NOTICE)]
        );
    }
    Ok(())
}

fn message(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        encrypted_content: None,
        status: None,
        id: None,
        role: role.to_string(),
        content: vec![if role == "assistant" {
            ContentItem::OutputText {
                text: text.to_string(),
                annotations: None,
                logprobs: None,
            }
        } else {
            ContentItem::InputText {
                text: text.to_string(),
            }
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

/// A thread written before encryption owners were kept: an OpenAI thread's
/// encrypted reasoning, compaction summary, agent message and tool output.
fn legacy_history() -> Vec<ResponseItem> {
    vec![
        message("user", "What is in the box?"),
        ResponseItem::Compaction {
            id: None,
            encrypted_content: "gAAAA-compaction".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Reasoning {
            id: None,
            summary: Vec::new(),
            content: None,
            encrypted_content: Some("gAAAA-reasoning".to_string()),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Reasoning {
            id: None,
            summary: Vec::new(),
            content: Some(vec![ReasoningItemContent::ReasoningText {
                text: "Look inside.".to_string(),
            }]),
            encrypted_content: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCall {
            encrypted_content: None,
            status: None,
            id: None,
            name: "open_box".to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            encrypted_function_args: None,
            call_id: "open-1".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: Some("open-1".to_string()),
            name: None,
            namespace: None,
            output: FunctionCallOutputPayload::from_content_items(vec![
                FunctionCallOutputContentItem::InputText {
                    text: "A cat.".to_string(),
                },
                FunctionCallOutputContentItem::EncryptedContent {
                    encrypted_content: "gAAAA-output".to_string(),
                },
            ]),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::AgentMessage {
            id: None,
            author: "/root/helper".to_string(),
            recipient: "/root".to_string(),
            content: vec![
                AgentMessageInputContent::InputText {
                    text: "It purrs.".to_string(),
                },
                AgentMessageInputContent::EncryptedContent {
                    encrypted_content: "gAAAA-agent".to_string(),
                },
            ],
            internal_chat_message_metadata_passthrough: None,
        },
        message("assistant", "A cat is in the box."),
    ]
}

fn write_legacy_rollout(dir: &TempDir) -> Result<PathBuf> {
    let thread_id = ThreadId::default();
    let meta = RolloutLine {
        timestamp: "2026-01-01T00:00:00.000Z".to_string(),
        ordinal: None,
        item: RolloutItem::SessionMeta(Box::new(SessionMetaLine {
            meta: SessionMeta {
                session_id: thread_id.into(),
                id: thread_id,
                parent_thread_id: None,
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                cwd: ".".into(),
                originator: "test_originator".to_string(),
                cli_version: "test_version".to_string(),
                model_provider: Some("openai".to_string()),
                ..Default::default()
            },
            git: None,
        })),
    };
    let path = dir.path().join("legacy-thread.jsonl");
    let mut file = std::fs::File::create(&path)?;
    writeln!(file, "{}", serde_json::to_string(&meta)?)?;
    for (at, item) in legacy_history().into_iter().enumerate() {
        let line = RolloutLine {
            timestamp: format!("2026-01-01T00:00:{:02}.000Z", at + 1),
            ordinal: None,
            item: RolloutItem::ResponseItem(item.into()),
        };
        writeln!(file, "{}", serde_json::to_string(&line)?)?;
    }
    Ok(path)
}

async fn resume_legacy(
    server: &MockServer,
    mut builder: TestCodexBuilder,
) -> Result<(TestCodex, TempDir)> {
    let dir = TempDir::new()?;
    let rollout = write_legacy_rollout(&dir)?;
    let test = builder
        .resume(server, Arc::new(TempDir::new()?), rollout)
        .await?;
    Ok((test, dir))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_history_of_another_provider_is_left_out() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse_completed("resp-1"),
            responses::sse_completed("resp-2"),
        ],
    )
    .await;
    let (test, _dir) = resume_legacy(&server, on("deepseek", "key-ds", None)).await?;
    test.submit_text_turn("And now?").await?;
    test.submit_text_turn("Once more.").await?;

    for request in &mock.requests() {
        let body = request.body_json().to_string();
        assert!(!body.contains("gAAAA"), "encrypted content sent: {body}");
        assert!(request.inputs_of_type("compaction").is_empty());
        // Reasoning without encrypted content stays.
        let reasoning = request.inputs_of_type("reasoning");
        assert_eq!(reasoning.len(), 1, "{reasoning:?}");
        assert_eq!(reasoning[0]["content"][0]["text"], "Look inside.");
        // The call keeps its output, the encrypted part a placeholder.
        assert_eq!(
            request.function_call_output("open-1")["output"],
            json!([
                {"type": "input_text", "text": "A cat."},
                {"type": "input_text", "text": PLACEHOLDER},
            ])
        );
        let agent_messages: Vec<Value> = request.inputs_of_type("agent_message");
        assert_eq!(agent_messages.len(), 1, "{agent_messages:?}");
        assert_eq!(
            agent_messages[0]["content"],
            json!([
                {"type": "input_text", "text": "It purrs."},
                {"type": "input_text", "text": PLACEHOLDER},
            ])
        );
        assert_eq!(notices(request), vec![notice(DEFAULT_NOTICE)]);
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_history_of_the_same_provider_is_kept() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_once(&server, responses::sse_completed("resp-1")).await;
    // The account may differ from the one that wrote it: that cannot be told.
    let (test, _dir) = resume_legacy(&server, on("openai", "another-key", None)).await?;
    test.submit_text_turn("And now?").await?;

    let request = mock.single_request();
    assert_eq!(notices(&request), Vec::<String>::new());
    let body = request.body_json().to_string();
    for encrypted in ["gAAAA-compaction", "gAAAA-reasoning", "gAAAA-output"] {
        assert!(body.contains(encrypted), "{encrypted} missing: {body}");
    }
    assert!(!body.contains(PLACEHOLDER));
    Ok(())
}
