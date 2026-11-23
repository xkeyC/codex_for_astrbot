//! AstrBot: code mode for an open-weight model (`open_model = true`): `exec`
//! offered as `run_code({code, description})`, with the same runtime.

use anyhow::Result;
use codex_config::model_provider_options::ModelProviderOptions;
use codex_core::StartThreadOptions;
use codex_features::Feature;
use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem;
use codex_protocol::dynamic_tools::DynamicToolCallRequest;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceTool;
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
use std::sync::Mutex;
use std::time::Duration;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

fn tools() -> Vec<DynamicToolSpec> {
    vec![
        DynamicToolSpec::Function(DynamicToolFunctionSpec {
            name: "lookup".to_string(),
            description: "Looks a key up.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "key": { "type": "string" } },
                "required": ["key"],
                "additionalProperties": false,
            }),
            defer_loading: false,
        }),
        DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
            name: "astrbot".to_string(),
            description: "Host tools.".to_string(),
            tools: vec![DynamicToolNamespaceTool::Function(
                DynamicToolFunctionSpec {
                    name: "vault".to_string(),
                    description: "Opens the secret vault.".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": { "code": { "type": "number" } },
                    }),
                    defer_loading: true,
                },
            )],
        }),
    ]
}

async fn open_model_thread(server: &MockServer) -> Result<TestCodex> {
    let base_test = test_codex()
        .with_model("test-gpt-5.1-codex")
        .with_config(|config| {
            config
                .features
                .enable(Feature::CodeModeOnly)
                .expect("code mode should be enabled");
            config.agents_enabled = false;
            config.model_provider_options.insert(
                config.model_provider_id.clone(),
                ModelProviderOptions {
                    open_model: true,
                    ..Default::default()
                },
            );
        })
        .build_with_auto_env(server)
        .await?;
    let new_thread = base_test
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: tools(),
            ..StartThreadOptions::new(base_test.config.clone())
        })
        .await?;
    let mut test = base_test;
    test.codex = new_thread.thread;
    test.session_configured = new_thread.session_configured;
    Ok(test)
}

fn run_code_call(call_id: &str, code: &str, description: &str) -> Value {
    responses::ev_function_call(
        call_id,
        "run_code",
        &json!({ "code": code, "description": description }).to_string(),
    )
}

async fn start_turn(test: &TestCodex, text: &str) -> Result<()> {
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: text.to_string(),
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

/// The text of each output item of the call, of either output type.
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

fn tool<'a>(request: &'a Value, name: &str) -> Option<&'a Value> {
    request["tools"]
        .as_array()?
        .iter()
        .find(|tool| tool["name"] == name)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_code_runs_the_program_and_answers_as_a_function() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                responses::ev_response_created("resp-1"),
                run_code_call(
                    "call-1",
                    "const value = await tools.lookup({ key: \"A\" });\n\
                     notify(\"looked up\");\ninspect({ value });",
                    "Look up A",
                ),
                responses::ev_completed("resp-1"),
            ]),
            sse(vec![
                responses::ev_assistant_message("msg-1", "done"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let test = open_model_thread(&server).await?;
    start_turn(&test, "Look up A.").await?;
    let request = next_tool_request(&test).await;
    assert_eq!(request.tool, "lookup");
    assert_eq!(request.arguments, json!({ "key": "A" }));
    answer(&test, request, "value-A").await?;
    turn_complete(&test).await;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let first = requests[0].body_json();
    let run_code = tool(&first, "run_code").expect("run_code is offered");
    assert_eq!(run_code["type"], "function");
    assert_eq!(
        run_code["parameters"]["required"],
        json!(["code", "description"])
    );
    let description = run_code["description"].as_str().expect("description");
    assert!(description.contains("## Writing code for run_code"));
    assert!(description.contains("interface ToolArgsMap {"));
    assert!(description.contains("  /** Looks a key up. */\n  \"lookup\": { key: string; };"));
    assert!(description.contains("declare function inspect(...values: unknown[]): void;"));
    // Deferred tools are found with the nested search, which the model is
    // not offered directly.
    assert!(description.contains("await tools.tool_search("));
    assert!(!description.contains("\"astrbot__vault\""));
    assert!(tool(&first, "exec").is_none());
    assert!(tool(&first, "tool_search").is_none());
    assert!(
        !first["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .any(|tool| tool["type"] == "tool_search")
    );
    let wait = tool(&first, "wait").expect("wait is offered");
    assert!(
        wait["description"]
            .as_str()
            .is_some_and(|text| text.contains("`run_code`") && !text.contains("`exec`"))
    );

    let input = requests[1].input();
    assert_eq!(
        outputs(&input, "call-1", "custom_tool_call_output"),
        Vec::<String>::new()
    );
    // `notify` gives a function call output, merged into the call's one
    // output in the request, before the program's result.
    let outputs = outputs(&input, "call-1", "function_call_output");
    assert_eq!(outputs.len(), 1, "{outputs:?}");
    assert!(
        outputs[0].starts_with("looked up\nScript completed")
            && outputs[0].contains("{\n  \"value\": \"value-A\"\n}"),
        "{outputs:?}"
    );
    Ok(())
}

/// Answers the `run_code` call, then `wait`s on the cell it yields, then ends.
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
                     const value = await tools.lookup({ key: \"slow\" });\ninspect(value);",
                    "Look up a slow key",
                ),
                responses::ev_completed("resp-1"),
            ]),
            1 => {
                let body: Value = serde_json::from_slice(&request.body).expect("JSON body");
                let input = body["input"].as_array().cloned().unwrap_or_default();
                let output = outputs(&input, "call-1", "function_call_output")
                    .into_iter()
                    .next()
                    .unwrap_or_default();
                let cell_id = output
                    .strip_prefix("Script running with cell ID ")
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
            _ => sse(vec![
                responses::ev_assistant_message("msg-1", "done"),
                responses::ev_completed("resp-3"),
            ]),
        };
        *served += 1;
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(body)
    }
}

async fn responses_requests(server: &MockServer) -> Vec<ResponsesRequest> {
    responses::received_responses_requests(server).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_program_yields_and_is_collected_with_wait() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(YieldThenWait {
            served: Mutex::new(0),
        })
        .mount(&server)
        .await;
    let test = open_model_thread(&server).await?;
    start_turn(&test, "Look up the slow key.").await?;
    let request = next_tool_request(&test).await;
    // The model gets control back while the call is still pending.
    tokio::time::timeout(Duration::from_secs(20), async {
        while responses_requests(&server).await.len() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    answer(&test, request, "slow-value").await?;
    turn_complete(&test).await;

    let requests = responses_requests(&server).await;
    assert_eq!(requests.len(), 3);
    let yielded = outputs(&requests[1].input(), "call-1", "function_call_output");
    assert!(
        yielded
            .first()
            .is_some_and(|output| output.starts_with("Script running with cell ID ")),
        "{yielded:?}"
    );
    let collected = outputs(&requests[2].input(), "call-2", "function_call_output");
    assert!(
        collected.iter().any(|output| output.contains("slow-value")),
        "{collected:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferred_tools_are_found_with_the_search_binding() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                responses::ev_response_created("resp-1"),
                run_code_call(
                    "call-1",
                    "const found = await tools.tool_search({ query: \"secret vault\" });\n\
                     inspect(found.tools.map((tool) => tool.name));\n\
                     inspect(found.tools[0].description);\n\
                     inspect(await tools[found.tools[0].name]({ code: 7 }));",
                    "Find and open the vault",
                ),
                responses::ev_completed("resp-1"),
            ]),
            sse(vec![
                responses::ev_assistant_message("msg-1", "done"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let test = open_model_thread(&server).await?;
    start_turn(&test, "Open the vault.").await?;
    // The found tool is callable in the same program.
    let request = next_tool_request(&test).await;
    assert_eq!(request.tool, "vault");
    answer(&test, request, "opened").await?;
    turn_complete(&test).await;

    let requests = mock.requests();
    let output = outputs(&requests[1].input(), "call-1", "function_call_output").join("\n");
    assert!(output.contains("\"astrbot__vault\""), "{output}");
    assert!(
        output.contains(
            "interface ToolArgsMap {\n  /** Opens the secret vault. */\n  \"astrbot__vault\": { code?: number; };\n}"
        ),
        "{output}"
    );
    assert!(output.contains("opened"), "{output}");
    Ok(())
}
