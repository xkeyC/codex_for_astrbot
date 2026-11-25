//! AstrBot: a provider on the chat wire
//! (`[model_provider_options.<id>] wire = "chat"`), against a mocked
//! `/chat/completions`.

use anyhow::Result;
use codex_config::model_provider_options::ModelProviderOptions;
use codex_config::model_provider_options::ProviderWire;
use codex_protocol::protocol::EventMsg;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use wiremock::Mock;
use wiremock::Request;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

/// Answers each request with the next chat completion stream.
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

fn chat_stream(id: &str, reasoning: &str, text: &str) -> String {
    let chunks = [
        json!({"id": id, "choices": [{"index": 0, "delta": {"role": "assistant", "reasoning_content": reasoning}}]}),
        json!({"id": id, "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": "stop"}]}),
        json!({"id": id, "choices": [], "usage": {"prompt_tokens": 1200, "completion_tokens": 9,
               "total_tokens": 1209, "prompt_cache_hit_tokens": 1024}}),
    ];
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    body
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chat_provider_answers_and_gets_its_reasoning_back() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    Mock::given(method("POST"))
        .and(path_regex(".*/chat/completions$"))
        .respond_with(ChatStreams {
            bodies: vec![
                chat_stream("chatcmpl-1", "A greeting.", "Hello!"),
                chat_stream("chatcmpl-2", "Again.", "Hi again!"),
            ],
            served: Arc::new(AtomicUsize::new(0)),
        })
        .mount(&server)
        .await;

    let mut builder = test_codex().with_config(|config| {
        let mut extra_body = serde_json::Map::new();
        extra_body.insert("thinking".to_string(), json!({"type": "enabled"}));
        config.model_provider_options.insert(
            config.model_provider_id.clone(),
            ModelProviderOptions {
                wire: ProviderWire::Chat,
                extra_body,
                ..Default::default()
            },
        );
    });
    let test = builder.build(&server).await?;

    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hi".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let mut usage = None;
    wait_for_event(&test.codex, |msg| {
        if let EventMsg::TokenCount(count) = msg
            && count.info.is_some()
        {
            usage = count.info.clone();
        }
        matches!(msg, EventMsg::TurnComplete(_))
    })
    .await;
    let usage = usage.expect("token usage");
    assert_eq!(usage.last_token_usage.input_tokens, 1200);
    assert_eq!(usage.last_token_usage.cached_input_tokens, 1024);
    test.submit_text_turn("and again").await?;

    let requests: Vec<Value> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path().ends_with("/chat/completions"))
        .map(|request| request.body_json().unwrap())
        .collect();
    assert_eq!(requests.len(), 2, "no /responses request: {requests:?}");
    let first = &requests[0];
    assert_eq!(first["stream"], json!(true));
    assert_eq!(first["stream_options"], json!({"include_usage": true}));
    assert_eq!(first["thinking"], json!({"type": "enabled"}));
    assert_eq!(first["messages"][0]["role"], json!("system"));
    let user_texts: Vec<&str> = first["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .collect();
    assert_eq!(user_texts.last(), Some(&"hi"));

    // The second request carries the first answer, with its reasoning.
    let messages = requests[1]["messages"].as_array().unwrap();
    let answer = messages
        .iter()
        .find(|message| message["role"] == "assistant")
        .expect("the first answer");
    assert_eq!(answer["content"], json!("Hello!"));
    assert_eq!(answer["reasoning_content"], json!("A greeting."));
    assert_eq!(
        messages.last().unwrap()["content"],
        json!("and again"),
        "{messages:?}"
    );
    Ok(())
}

/// Against a live chat provider (run with `--ignored`):
/// `CODEX_LIVE_CHAT_BASE_URL`, `CODEX_LIVE_CHAT_KEY_FILE` (a file holding the
/// key), `CODEX_LIVE_CHAT_MODEL`, optional `CODEX_LIVE_CHAT_EXTRA_BODY` (JSON)
/// and `CODEX_LIVE_CHAT_EFFORT`. A function tool is called and answered, and a
/// second turn reads the first (its reasoning sent back).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "talks to a live provider"]
async fn live_chat_provider_calls_a_tool_and_remembers() -> Result<()> {
    use codex_core::StartThreadOptions;
    use codex_model_provider_info::ModelProviderInfo;
    use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem;
    use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
    use codex_protocol::dynamic_tools::DynamicToolResponse;
    use codex_protocol::dynamic_tools::DynamicToolSpec;
    use codex_protocol::protocol::Op;
    use std::time::Instant;

    let env = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
    let (Some(base_url), Some(key_file), Some(model)) = (
        env("CODEX_LIVE_CHAT_BASE_URL"),
        env("CODEX_LIVE_CHAT_KEY_FILE"),
        env("CODEX_LIVE_CHAT_MODEL"),
    ) else {
        eprintln!("CODEX_LIVE_CHAT_* not set; skipped");
        return Ok(());
    };
    let key = std::fs::read_to_string(key_file)?.trim().to_string();
    let extra_body: serde_json::Map<String, Value> = env("CODEX_LIVE_CHAT_EXTRA_BODY")
        .map(|body| serde_json::from_str(&body))
        .transpose()?
        .unwrap_or_default();
    let effort = env("CODEX_LIVE_CHAT_EFFORT");

    let server = start_mock_server().await;
    let provider: ModelProviderInfo = serde_json::from_value(json!({
        "name": "live",
        "base_url": base_url,
        "experimental_bearer_token": key,
        "wire_api": "responses",
        "request_max_retries": 0,
        "stream_max_retries": 0,
        "stream_idle_timeout_ms": 60000,
    }))?;
    let mut builder = test_codex().with_config(move |config| {
        config.model_provider_id = "live".to_string();
        config.model_provider = provider;
        config.model = Some(model);
        if let Some(effort) = effort {
            config.model_reasoning_effort = serde_json::from_value(json!(effort)).ok();
        }
        config.model_provider_options.insert(
            "live".to_string(),
            ModelProviderOptions {
                wire: ProviderWire::Chat,
                extra_body,
                ..Default::default()
            },
        );
    });
    let base = builder.build(&server).await?;
    let backend_task = DynamicToolSpec::Function(DynamicToolFunctionSpec {
        name: "backend_task".to_string(),
        description: "Hand a task to the backend (it can look things up) and get its answer."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {"task": {"type": "string", "description": "What to do."}},
            "required": ["task"],
            "additionalProperties": false,
        }),
        defer_loading: false,
    });
    let thread = base
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: vec![backend_task],
            ..StartThreadOptions::new(base.config.clone())
        })
        .await?;
    let codex = thread.thread;

    let turn = |text: &str| {
        TurnInputRequest::user_input(vec![UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }])
    };
    let started = Instant::now();
    codex
        .start_or_steer_turn(turn(
            "Use the backend_task tool to ask the backend for today's weather in Shanghai, \
             then tell me what it said in one short sentence.",
        ))
        .await?;
    let mut first = None;
    let answered = loop {
        let event = codex.next_event().await?;
        match event.msg {
            EventMsg::DynamicToolCallRequest(request) => {
                eprintln!(
                    "tool call after {:?}: {} {}",
                    started.elapsed(),
                    request.tool,
                    request.arguments
                );
                codex
                    .submit(Op::DynamicToolResponse {
                        id: request.call_id,
                        response: DynamicToolResponse {
                            content_items: vec![DynamicToolCallOutputContentItem::InputText {
                                text: "Shanghai today: 24°C, cloudy, light wind.".to_string(),
                            }],
                            success: true,
                            end_turn: false,
                            speak: None,
                        },
                    })
                    .await?;
            }
            EventMsg::AgentMessageContentDelta(_) if first.is_none() => {
                first = Some(started.elapsed());
            }
            EventMsg::TokenCount(count) => {
                if let Some(info) = count.info {
                    eprintln!("usage: {:?}", info.last_token_usage);
                }
            }
            EventMsg::Error(error) => panic!("turn failed: {}", error.message),
            EventMsg::TurnComplete(done) => break done.last_agent_message,
            _ => {}
        }
    };
    eprintln!(
        "turn 1 in {:?} (first text {first:?}): {answered:?}",
        started.elapsed()
    );
    assert!(answered.unwrap_or_default().contains("24"));

    let started = Instant::now();
    codex
        .start_or_steer_turn(turn(
            "Which city did I ask about? Answer with the city name only.",
        ))
        .await?;
    let answer = loop {
        let event = codex.next_event().await?;
        match event.msg {
            EventMsg::Error(error) => panic!("turn failed: {}", error.message),
            EventMsg::TurnComplete(done) => break done.last_agent_message.unwrap_or_default(),
            _ => {}
        }
    };
    eprintln!("turn 2 in {:?}: {answer:?}", started.elapsed());
    assert!(
        answer.contains("Shanghai") || answer.contains("上海"),
        "{answer}"
    );
    Ok(())
}

/// A chat completion with signed thinking, as LiteLLM gives Anthropic's.
fn thinking_stream(id: &str, thinking: &str, signature: &str, text: &str) -> String {
    let chunks = [
        json!({"id": id, "choices": [{"index": 0, "delta": {"role": "assistant", "reasoning_content": thinking,
            "thinking_blocks": [{"type": "thinking", "thinking": thinking, "index": 0}]}}]}),
        json!({"id": id, "choices": [{"index": 0, "delta": {"reasoning_content": "",
            "thinking_blocks": [{"type": "thinking", "thinking": "", "signature": signature, "index": 0}]}}]}),
        json!({"id": id, "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": "stop"}]}),
        json!({"id": id, "choices": [], "usage": {"prompt_tokens": 50, "completion_tokens": 5, "total_tokens": 55}}),
    ];
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    body
}

fn chat_on(provider: &str) -> core_test_support::test_codex::TestCodexBuilder {
    let provider = provider.to_string();
    test_codex()
        .with_auth(codex_login::CodexAuth::from_api_key("key"))
        .with_config(move |config| {
            config.model_provider_id = provider.clone();
            config
                .model_providers
                .insert(provider.clone(), config.model_provider.clone());
            config.model_provider_options.insert(
                provider,
                ModelProviderOptions {
                    wire: ProviderWire::Chat,
                    ..Default::default()
                },
            );
        })
}

fn assistant_messages(request: &Value) -> Vec<Value> {
    request["messages"]
        .as_array()
        .expect("chat messages")
        .iter()
        .filter(|message| message["role"] == "assistant")
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_thinking_goes_back_whole_to_its_provider_only() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    Mock::given(method("POST"))
        .and(path_regex(".*/chat/completions$"))
        .respond_with(ChatStreams {
            bodies: vec![
                thinking_stream("chatcmpl-1", "Plan the greeting.", "SIG-ONE", "Hello!"),
                thinking_stream("chatcmpl-2", "Again.", "SIG-TWO", "Hi again!"),
                thinking_stream("chatcmpl-3", "Other.", "SIG-THREE", "Hey."),
            ],
            served: Arc::new(AtomicUsize::new(0)),
        })
        .mount(&server)
        .await;

    let test = chat_on("gateway").build(&server).await?;
    test.submit_text_turn("hi").await?;
    test.submit_text_turn("and again").await?;
    test.codex.shutdown_and_wait().await?;
    let path = test.codex.rollout_path().expect("rollout path");
    // The same history on another provider (or key): its thinking is foreign.
    let test = chat_on("elsewhere")
        .resume(&server, test.home.clone(), path)
        .await?;
    test.submit_text_turn("third").await?;

    let requests: Vec<Value> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path().ends_with("/chat/completions"))
        .map(|request| request.body_json().unwrap())
        .collect();
    assert_eq!(requests.len(), 3);
    // The prompt cache needs the history unchanged: what the first request
    // sent is where the second begins, byte for byte, and so are the tools.
    let first = requests[0]["messages"].as_array().unwrap();
    let second = requests[1]["messages"].as_array().unwrap();
    assert_eq!(&second[..first.len()], &first[..], "{second:?}");
    assert_eq!(requests[0]["tools"], requests[1]["tools"]);
    let back = assistant_messages(&requests[1]);
    assert_eq!(back.len(), 1, "{back:?}");
    assert_eq!(
        back[0]["thinking_blocks"],
        json!([{"type": "thinking", "thinking": "Plan the greeting.", "signature": "SIG-ONE"}])
    );
    assert_eq!(back[0]["reasoning_content"], json!("Plan the greeting."));
    assert_eq!(back[0]["content"], json!("Hello!"));
    // Another provider gets the answers, not the thinking it cannot verify.
    let third = assistant_messages(&requests[2]);
    assert_eq!(third.len(), 2, "{third:?}");
    assert!(
        third.iter().all(|message| message.get("thinking_blocks").is_none()),
        "{third:?}"
    );
    Ok(())
}
