//! AstrBot: `features.code_mode.host_namespace = "astrbot"`: in code mode
//! (exec and run_code) the host's tools drop the root, keep the plugin layer
//! (a plugin's namesake tool merges into one name where it is free), and
//! share no name with Codex's tools (`astrbot_` / `codex_`).

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
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use wiremock::MockServer;

fn namespace(name: &str, description: &str, tools: &[(&str, &str, bool)]) -> DynamicToolSpec {
    DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
        name: name.to_string(),
        description: description.to_string(),
        tools: tools
            .iter()
            .map(|(tool, description, deferred)| {
                DynamicToolNamespaceTool::Function(DynamicToolFunctionSpec {
                    name: (*tool).to_string(),
                    description: (*description).to_string(),
                    input_schema: json!({"type": "object", "properties": {}}),
                    defer_loading: *deferred,
                })
            })
            .collect(),
    })
}

/// AstrBot's own tools in `astrbot`, plugins' in `astrbot__<plugin>`.
fn host_tools() -> Vec<DynamicToolSpec> {
    vec![
        namespace(
            "astrbot",
            "AstrBot tools.",
            &[
                ("lookup_order_status", "Looks an order up.", false),
                // Named like Codex's own nested search.
                ("tool_search", "Finds things.", false),
                // Takes the name the plugin below would merge into.
                ("deepseek_research", "Researches at the root.", false),
            ],
        ),
        namespace(
            "astrbot__vrchat",
            "VRChat plugin.",
            &[("vrchat_chatbox", "Writes to the VRChat chatbox.", true)],
        ),
        namespace(
            "astrbot__qq_file",
            "QQ files.",
            &[("qq_file", "Sends a file.", false)],
        ),
        namespace(
            "astrbot__deepseek_research",
            "Research plugin.",
            &[("deepseek_research", "Researches in the plugin.", false)],
        ),
    ]
}

async fn thread(server: &MockServer, open_model: bool) -> Result<TestCodex> {
    let base_test = test_codex()
        .with_model("test-gpt-5.1-codex")
        .with_config(move |config| {
            config
                .features
                .enable(Feature::CodeModeOnly)
                .expect("code mode should be enabled");
            config.agents_enabled = false;
            config.code_mode.tool_catalog = true;
            config.code_mode.host_namespace = Some("astrbot".to_string());
            config.model_provider_options.insert(
                config.model_provider_id.clone(),
                ModelProviderOptions {
                    open_model,
                    ..Default::default()
                },
            );
        })
        .build_with_auto_env(server)
        .await?;
    let new_thread = base_test
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: host_tools(),
            ..StartThreadOptions::new(base_test.config.clone())
        })
        .await?;
    let mut test = base_test;
    test.codex = new_thread.thread;
    test.session_configured = new_thread.session_configured;
    Ok(test)
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

async fn answer_next(test: &TestCodex, text: &str) -> Result<DynamicToolCallRequest> {
    let request = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::DynamicToolCallRequest(request) => Some(request.clone()),
        _ => None,
    })
    .await;
    test.codex
        .submit(Op::DynamicToolResponse {
            id: request.call_id.clone(),
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
    Ok(request)
}

async fn turn_complete(test: &TestCodex) {
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
}

/// The text of the call's outputs, of either kind.
fn output_text(input: &[Value], call_id: &str) -> String {
    input
        .iter()
        .filter(|item| {
            item["call_id"] == call_id
                && item["type"]
                    .as_str()
                    .is_some_and(|ty| ty.ends_with("_output"))
        })
        .map(|item| match &item["output"] {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_description(request: &Value, name: &str) -> String {
    request["tools"]
        .as_array()
        .and_then(|tools| tools.iter().find(|tool| tool["name"] == name))
        .and_then(|tool| tool["description"].as_str())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_calls_host_tools_by_their_own_names() -> Result<()> {
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
                    "text(ALL_TOOLS.map((tool) => tool.name).sort().join(\",\"));\n\
                     text(await tools.lookup_order_status({}));\n\
                     text(await tools.qq_file({}));\n\
                     text(await tools.deepseek_research__deepseek_research({}));",
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
    let test = thread(&server, /*open_model*/ false).await?;
    start_turn(&test, "Check my order.").await?;
    let lookup = answer_next(&test, "shipped").await?;
    let qq_file = answer_next(&test, "file sent").await?;
    let research = answer_next(&test, "researched").await?;
    turn_complete(&test).await;

    // Dispatch goes to the real tools.
    assert_eq!(
        (lookup.namespace.as_deref(), lookup.tool.as_str()),
        (Some("astrbot"), "lookup_order_status")
    );
    assert_eq!(
        (qq_file.namespace.as_deref(), qq_file.tool.as_str()),
        (Some("astrbot__qq_file"), "qq_file")
    );
    assert_eq!(
        (research.namespace.as_deref(), research.tool.as_str()),
        (Some("astrbot__deepseek_research"), "deepseek_research")
    );

    let requests = mock.requests();
    let first = requests[0].body_json();
    let exec = tool_description(&first, "exec");
    assert!(exec.contains("### `lookup_order_status`"), "{exec}");
    assert!(exec.contains("declare const tools: { qq_file("), "{exec}");
    assert!(!exec.contains("tools: { astrbot__"), "{exec}");
    // The catalog lists whole names under plain headings.
    let catalog = requests[0]
        .message_input_texts("developer")
        .into_iter()
        .find(|text| text.starts_with("<tool_catalog>"))
        .expect("a tool catalog");
    assert!(
        catalog.contains(
            "vrchat — VRChat plugin.\n- vrchat__vrchat_chatbox: Writes to the VRChat chatbox.\n"
        ),
        "{catalog}"
    );
    assert!(
        catalog.contains("listed by name; headings only group them"),
        "{catalog}"
    );
    assert!(!catalog.contains("prefix"), "{catalog}");

    let output = output_text(&requests[1].input(), "call-1");
    let names = output
        .lines()
        .find(|line| line.contains(','))
        .unwrap_or_default();
    for name in [
        "deepseek_research",
        "deepseek_research__deepseek_research",
        "lookup_order_status",
        "qq_file",
        "vrchat__vrchat_chatbox",
    ] {
        assert!(
            names.split(',').any(|listed| listed == name),
            "{name} in {names}"
        );
    }
    assert!(!names.contains("astrbot__"), "{names}");
    for answer in ["shipped", "file sent", "researched"] {
        assert!(output.contains(answer), "{output}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_code_resolves_conflicts_and_finds_tools_by_final_names() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let program = "const finder = await tools.astrbot_tool_search({});\n\
        const found = await tools.codex_tool_search({ query: \"vrchat chatbox\" });\n\
        inspect(finder, found.tools.map((tool) => tool.name), found.tools[0].description);\n\
        inspect(await tools.qq_file({}));\n\
        inspect(typeof tools.tool_search, typeof tools.vrchat_chatbox);";
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_function_call(
                    "call-1",
                    "run_code",
                    &json!({ "code": program, "description": "Search" }).to_string(),
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
    let test = thread(&server, /*open_model*/ true).await?;
    start_turn(&test, "Find the chatbox.").await?;
    let finder = answer_next(&test, "finder answer").await?;
    let qq_file = answer_next(&test, "qq answer").await?;
    turn_complete(&test).await;

    assert_eq!(
        (finder.namespace.as_deref(), finder.tool.as_str()),
        (Some("astrbot"), "tool_search")
    );
    assert_eq!(
        (qq_file.namespace.as_deref(), qq_file.tool.as_str()),
        (Some("astrbot__qq_file"), "qq_file")
    );

    let requests = mock.requests();
    let sdk = tool_description(&requests[0].body_json(), "run_code");
    for key in [
        "\"lookup_order_status\":",
        "\"astrbot_tool_search\":",
        "\"codex_tool_search\":",
        "\"qq_file\":",
        "\"deepseek_research\":",
        "\"deepseek_research__deepseek_research\":",
    ] {
        assert!(sdk.contains(key), "{key} in {sdk}");
    }
    assert!(!sdk.contains("\"tool_search\":"), "{sdk}");
    assert!(!sdk.contains("astrbot__"), "{sdk}");
    assert!(sdk.contains("await tools.codex_tool_search({"), "{sdk}");

    let output = output_text(&requests[1].input(), "call-1");
    assert!(output.contains("finder answer"), "{output}");
    assert!(output.contains("\"vrchat__vrchat_chatbox\""), "{output}");
    assert!(
        output.contains(
            "interface ToolArgsMap {\n  /** Writes to the VRChat chatbox. */\n  \"vrchat__vrchat_chatbox\":"
        ),
        "{output}"
    );
    assert!(output.contains("qq answer"), "{output}");
    assert!(output.contains("undefined\nundefined"), "{output}");
    Ok(())
}
