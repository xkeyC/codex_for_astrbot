//! AstrBot: a provider other than OpenAI gets one output per call: code
//! mode's `notify()` outputs are merged into the call's output (DeepSeek
//! refuses a duplicate tool output).

use anyhow::Result;
use codex_config::model_provider_options::ModelProviderOptions;
use codex_config::model_provider_options::ProviderWire;
use codex_core::StartThreadOptions;
use codex_features::Feature;
use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem;
use codex_protocol::dynamic_tools::DynamicToolCallRequest;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolResponse;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

#[derive(Clone, Copy, PartialEq)]
enum Provider {
    /// The test provider, which Codex takes for OpenAI.
    OpenAi,
    /// A provider by another name (DeepSeek), on the given wire.
    Other(ProviderWire),
}

async fn thread(server: &MockServer, provider: Provider, open_model: bool) -> Result<TestCodex> {
    let base_test = test_codex()
        .with_model("test-gpt-5.1-codex")
        .with_config(move |config| {
            config
                .features
                .enable(Feature::CodeModeOnly)
                .expect("code mode should be enabled");
            config.agents_enabled = false;
            let wire = match provider {
                Provider::OpenAi => ProviderWire::Responses,
                Provider::Other(wire) => {
                    config.model_provider.name = "DeepSeek".to_string();
                    wire
                }
            };
            config.model_provider_options.insert(
                config.model_provider_id.clone(),
                ModelProviderOptions {
                    open_model,
                    wire,
                    ..Default::default()
                },
            );
        })
        .build_with_auto_env(server)
        .await?;
    let new_thread = base_test
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: vec![DynamicToolSpec::Function(DynamicToolFunctionSpec {
                name: "lookup".to_string(),
                description: "Looks a key up.".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
                defer_loading: false,
            })],
            ..StartThreadOptions::new(base_test.config.clone())
        })
        .await?;
    let mut test = base_test;
    test.codex = new_thread.thread;
    test.session_configured = new_thread.session_configured;
    Ok(test)
}

async fn start_turn(test: &TestCodex) -> Result<()> {
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Go.".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    Ok(())
}

async fn next_tool_request(test: &TestCodex) -> DynamicToolCallRequest {
    wait_for_event_match(&test.codex, |event| match event {
        EventMsg::DynamicToolCallRequest(request) => Some(request.clone()),
        _ => None,
    })
    .await
}

async fn answer(test: &TestCodex, request: DynamicToolCallRequest, text: &str) -> Result<()> {
    test.codex
        .submit(Op::DynamicToolResponse {
            id: request.call_id,
            response: DynamicToolResponse {
                content_items: vec![DynamicToolCallOutputContentItem::InputText {
                    text: text.to_string(),
                }],
                success: true,
                end_turn: false,
                speak: None,
            },
        })
        .await?;
    Ok(())
}

async fn turn_complete(test: &TestCodex) {
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
}

/// Each output item of the call, as its text.
fn outputs(input: &[Value], call_id: &str, ty: &str) -> Vec<String> {
    input
        .iter()
        .filter(|item| item["type"] == ty && item["call_id"] == call_id)
        .map(|item| match &item["output"] {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            other => other.to_string(),
        })
        .collect()
}

fn run_code_call(call_id: &str, code: &str) -> Value {
    responses::ev_function_call(
        call_id,
        "run_code",
        &json!({ "code": code, "description": "Work" }).to_string(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_notifying_program_has_one_output() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                responses::ev_response_created("resp-1"),
                run_code_call("call-1", "notify(\"working on it\");\ntext(\"all done\");"),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse_completed("resp-2"),
        ],
    )
    .await;
    let test = thread(&server, Provider::Other(ProviderWire::Responses), true).await?;
    start_turn(&test).await?;
    turn_complete(&test).await;

    let input = mock.requests()[1].input();
    let outputs = outputs(&input, "call-1", "function_call_output");
    assert_eq!(outputs.len(), 1, "{input:?}");
    // The notification came first, and reads first.
    assert!(
        outputs[0].starts_with("working on it\nScript completed")
            && outputs[0].ends_with("all done"),
        "{outputs:?}"
    );
    Ok(())
}

/// Answers the `run_code` call, `wait`s on the cell it yields, then ends.
struct YieldThenWait {
    served: Mutex<usize>,
}

impl Respond for YieldThenWait {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut served = self.served.lock().expect("lock");
        let body = match *served {
            0 => sse(vec![
                responses::ev_response_created("resp-1"),
                run_code_call(
                    "call-1",
                    "// @exec: {\"yield_time_ms\": 200}\n\
                     const value = await tools.lookup({});\n\
                     notify(\"got it\");\ntext(value);",
                ),
                responses::ev_completed("resp-1"),
            ]),
            1 => {
                let body: Value = serde_json::from_slice(&request.body).expect("JSON body");
                let input = body["input"].as_array().cloned().unwrap_or_default();
                let cell_id = outputs(&input, "call-1", "function_call_output")
                    .first()
                    .and_then(|output| output.strip_prefix("Script running with cell ID "))
                    .and_then(|rest| rest.split('\n').next())
                    .unwrap_or("missing")
                    .to_string();
                sse(vec![
                    responses::ev_response_created("resp-2"),
                    responses::ev_function_call(
                        "call-2",
                        "wait",
                        &json!({ "cell_id": cell_id, "yield_time_ms": 10000 }).to_string(),
                    ),
                    responses::ev_completed("resp-2"),
                ])
            }
            _ => responses::sse_completed("resp-3"),
        };
        *served += 1;
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(body)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_yielded_program_that_notifies_keeps_one_output() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(YieldThenWait {
            served: Mutex::new(0),
        })
        .mount(&server)
        .await;
    let test = thread(&server, Provider::Other(ProviderWire::Responses), true).await?;
    start_turn(&test).await?;
    let request = next_tool_request(&test).await;
    tokio::time::timeout(Duration::from_secs(20), async {
        while responses::received_responses_requests(&server).await.len() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    answer(&test, request, "the value").await?;
    turn_complete(&test).await;

    let requests: Vec<ResponsesRequest> = responses::received_responses_requests(&server).await;
    assert_eq!(requests.len(), 3);
    let input = requests[2].input();
    // The yield, then what the program said after it.
    let first = outputs(&input, "call-1", "function_call_output");
    assert_eq!(first.len(), 1, "{input:?}");
    assert!(
        first[0].starts_with("Script running with cell ID ") && first[0].ends_with("got it"),
        "{first:?}"
    );
    let waited = outputs(&input, "call-2", "function_call_output");
    assert_eq!(waited.len(), 1, "{input:?}");
    assert!(waited[0].contains("the value"), "{waited:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_keeps_upstreams_separate_custom_outputs() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_custom_tool_call(
                    "call-1",
                    "exec",
                    "notify(\"working on it\");\ntext(\"all done\");",
                ),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse_completed("resp-2"),
        ],
    )
    .await;
    let test = thread(&server, Provider::OpenAi, false).await?;
    start_turn(&test).await?;
    turn_complete(&test).await;

    let input = mock.requests()[1].input();
    let outputs = outputs(&input, "call-1", "custom_tool_call_output");
    assert_eq!(outputs.len(), 2, "{input:?}");
    assert!(outputs.iter().any(|output| output == "working on it"));
    Ok(())
}

/// Answers each chat request with the next stream.
struct ChatStreams {
    bodies: Vec<String>,
    served: Arc<AtomicUsize>,
}

impl Respond for ChatStreams {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let at = self.served.fetch_add(1, Ordering::SeqCst);
        let body = self.bodies.get(at).or(self.bodies.last()).cloned();
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_raw(body.unwrap_or_default(), "text/event-stream")
    }
}

fn chat_stream(chunks: Vec<Value>) -> String {
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    body
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_chat_wire_gets_one_tool_message_per_call() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let arguments =
        json!({"code": "notify(\"working on it\");\ntext(\"all done\");", "description": "Work"})
            .to_string();
    Mock::given(method("POST"))
        .and(path_regex(".*/chat/completions$"))
        .respond_with(ChatStreams {
            bodies: vec![
                chat_stream(vec![
                    json!({"id": "chat-1", "choices": [{"index": 0, "delta": {"role": "assistant",
                        "tool_calls": [{"index": 0, "id": "call-1", "type": "function",
                        "function": {"name": "run_code", "arguments": arguments}}]}}]}),
                    json!({"id": "chat-1", "choices": [{"index": 0, "delta": {},
                        "finish_reason": "tool_calls"}]}),
                ]),
                chat_stream(vec![json!({"id": "chat-2", "choices": [{"index": 0,
                    "delta": {"role": "assistant", "content": "Done."}, "finish_reason": "stop"}]})]),
            ],
            served: Arc::new(AtomicUsize::new(0)),
        })
        .mount(&server)
        .await;
    let test = thread(&server, Provider::Other(ProviderWire::Chat), true).await?;
    start_turn(&test).await?;
    turn_complete(&test).await;

    let requests: Vec<Value> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path().ends_with("/chat/completions"))
        .map(|request| request.body_json().expect("JSON body"))
        .collect();
    assert_eq!(requests.len(), 2);
    let messages = requests[1]["messages"].as_array().expect("messages");
    let tool_messages = messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect::<Vec<_>>();
    assert_eq!(tool_messages.len(), 1, "{messages:?}");
    assert_eq!(tool_messages[0]["tool_call_id"], "call-1");
    let content = tool_messages[0]["content"].as_str().unwrap_or_default();
    assert!(
        content.starts_with("working on it\nScript completed") && content.ends_with("all done"),
        "{content}"
    );
    assert!(
        !messages.iter().any(|message| message["content"]
            .as_str()
            .is_some_and(|text| text.starts_with("(Tool output)"))),
        "{messages:?}"
    );
    Ok(())
}
