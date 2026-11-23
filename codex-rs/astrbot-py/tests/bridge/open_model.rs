//! AstrBot: a thread an OpenAI model left encrypted content in, continued on
//! a provider with `[model_provider_options.<id>] open_model = true`.

use anyhow::Result;
use codex_config::model_provider_options::ModelProviderOptions;
use codex_history::RolloutItem;
use codex_history::RolloutLine;
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
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

const NOTICE_TAG: &str = "<model_switch_notice>";
const PLACEHOLDER: &str = "[encrypted content from a previous model omitted]";

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

/// What an OpenAI model leaves in a thread: encrypted reasoning, a remote
/// compaction summary, an agent message and a tool output with encrypted
/// parts. `encrypted = false` gives the same thread without them.
fn openai_history(encrypted: bool) -> Vec<ResponseItem> {
    let mut items = vec![message("user", "What is in the box?")];
    if encrypted {
        items.extend([
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
        ]);
    }
    items.extend([
        // The open model's own reasoning (none encrypted) stays.
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
            output: FunctionCallOutputPayload::from_content_items(
                std::iter::once(FunctionCallOutputContentItem::InputText {
                    text: "A cat.".to_string(),
                })
                .chain(
                    encrypted.then(|| FunctionCallOutputContentItem::EncryptedContent {
                        encrypted_content: "gAAAA-output".to_string(),
                    }),
                )
                .collect(),
            ),
            internal_chat_message_metadata_passthrough: None,
        },
    ]);
    if encrypted {
        items.push(ResponseItem::AgentMessage {
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
        });
    }
    items.push(message("assistant", "A cat is in the box."));
    items
}

fn write_rollout(dir: &TempDir, items: Vec<ResponseItem>) -> Result<PathBuf> {
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
    let path = dir.path().join("openai-thread.jsonl");
    let mut file = std::fs::File::create(&path)?;
    writeln!(file, "{}", serde_json::to_string(&meta)?)?;
    for (at, item) in items.into_iter().enumerate() {
        let line = RolloutLine {
            timestamp: format!("2026-01-01T00:00:{:02}.000Z", at + 1),
            ordinal: None,
            item: RolloutItem::ResponseItem(item.into()),
        };
        writeln!(file, "{}", serde_json::to_string(&line)?)?;
    }
    Ok(path)
}

/// Resumes the thread on the test provider, with `options` for it.
async fn resume(
    server: &wiremock::MockServer,
    items: Vec<ResponseItem>,
    options: ModelProviderOptions,
) -> Result<(TestCodex, TempDir)> {
    let dir = TempDir::new()?;
    let rollout = write_rollout(&dir, items)?;
    let mut builder = test_codex().with_config(move |config| {
        config
            .model_provider_options
            .insert(config.model_provider_id.clone(), options);
    });
    let test = builder
        .resume(server, Arc::new(TempDir::new()?), rollout)
        .await?;
    Ok((test, dir))
}

fn open_model(notice: Option<&str>) -> ModelProviderOptions {
    ModelProviderOptions {
        open_model: true,
        open_model_switch_notice: notice.map(str::to_string),
        ..Default::default()
    }
}

fn input_of_type(request: &ResponsesRequest, ty: &str) -> Vec<Value> {
    request.inputs_of_type(ty)
}

/// The notices of the request, in order.
fn notices(request: &ResponsesRequest) -> Vec<String> {
    request
        .message_input_texts("user")
        .into_iter()
        .filter(|text| text.starts_with(NOTICE_TAG))
        .collect()
}

fn position_of_user_text(input: &[Value], text: &str) -> Option<usize> {
    input.iter().position(|item| {
        item["type"] == "message"
            && item["role"] == "user"
            && item["content"]
                .as_array()
                .is_some_and(|content| content.iter().any(|part| part["text"] == text))
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_open_model_gets_no_encrypted_history_and_one_notice() -> Result<()> {
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
    let (test, _dir) = resume(&server, openai_history(true), open_model(None)).await?;
    test.submit_text_turn("And now?").await?;
    test.submit_text_turn("Once more.").await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        let body = request.body_json().to_string();
        assert!(!body.contains("gAAAA"), "encrypted content sent: {body}");
        assert!(input_of_type(request, "compaction").is_empty());
        // Only the open model's own reasoning is left.
        let reasoning = input_of_type(request, "reasoning");
        assert_eq!(reasoning.len(), 1, "{reasoning:?}");
        assert_eq!(reasoning[0]["content"][0]["text"], "Look inside.");
        // The call keeps its output, the encrypted part a placeholder.
        assert_eq!(
            request.function_call_output("open-1")["output"],
            serde_json::json!([
                {"type": "input_text", "text": "A cat."},
                {"type": "input_text", "text": PLACEHOLDER},
            ])
        );
        let agent_messages = input_of_type(request, "agent_message");
        assert_eq!(agent_messages.len(), 1, "{agent_messages:?}");
        for agent_message in &agent_messages {
            assert_eq!(
                agent_message["content"],
                serde_json::json!([
                    {"type": "input_text", "text": "It purrs."},
                    {"type": "input_text", "text": PLACEHOLDER},
                ])
            );
        }
        // One notice, and not again on the next turn.
        assert_eq!(
            notices(request),
            vec![format!(
                "{NOTICE_TAG}\nNote: the model of this conversation was switched. Part of the \
earlier history was produced by the previous model in an encrypted form this model cannot read, \
so some earlier context and summaries may be missing. If you need earlier context, look it up in \
the message history.\n</model_switch_notice>"
            )]
        );
    }
    // The notice comes just before the first turn's message.
    let input = requests[0].input();
    let notice_at = input
        .iter()
        .position(|item| {
            item["role"] == "user"
                && item["content"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with(NOTICE_TAG))
        })
        .expect("the notice");
    assert_eq!(
        position_of_user_text(&input, "And now?"),
        Some(notice_at + 1)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_notice_text_can_be_replaced() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_once(&server, responses::sse_completed("resp-1")).await;
    let (test, _dir) = resume(
        &server,
        openai_history(true),
        open_model(Some("Some history is gone.")),
    )
    .await?;
    test.submit_text_turn("And now?").await?;

    assert_eq!(
        notices(&mock.single_request()),
        vec![format!(
            "{NOTICE_TAG}\nSome history is gone.\n</model_switch_notice>"
        )]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_notice_without_encrypted_history() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_once(&server, responses::sse_completed("resp-1")).await;
    let (test, _dir) = resume(&server, openai_history(false), open_model(None)).await?;
    test.submit_text_turn("And now?").await?;

    let request = mock.single_request();
    assert_eq!(notices(&request), Vec::<String>::new());
    assert_eq!(
        request.function_call_output("open-1")["output"],
        serde_json::json!([{"type": "input_text", "text": "A cat."}])
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_open_model_the_history_is_sent_as_before() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_once(&server, responses::sse_completed("resp-1")).await;
    let (test, _dir) = resume(
        &server,
        openai_history(true),
        ModelProviderOptions::default(),
    )
    .await?;
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
