use anyhow::Result;
use codex_core::StartThreadOptions;
use codex_core::TurnInputRequest;
use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolResponse;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;

fn tool(name: &str) -> DynamicToolSpec {
    DynamicToolSpec::Function(DynamicToolFunctionSpec {
        name: name.to_string(),
        description: format!("Does {name}."),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        }),
        defer_loading: false,
    })
}

async fn thread_with_tools(server: &wiremock::MockServer) -> Result<TestCodex> {
    let mut builder = test_codex();
    let base_test = builder.build_with_auto_env(server).await?;
    let new_thread = base_test
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: vec![tool("jump"), tool("look")],
            ..StartThreadOptions::new(base_test.config.clone())
        })
        .await?;
    let mut test = base_test;
    test.codex = new_thread.thread;
    test.session_configured = new_thread.session_configured;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Jump".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    Ok(test)
}

/// Answers the next dynamic tool call of the turn.
async fn answer(test: &TestCodex, end_turn: bool, speak: Option<&str>) -> Result<()> {
    let EventMsg::DynamicToolCallRequest(request) = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::DynamicToolCallRequest(_))
    })
    .await
    else {
        unreachable!("event guard guarantees DynamicToolCallRequest");
    };
    test.codex
        .submit(Op::DynamicToolResponse {
            id: request.call_id,
            response: DynamicToolResponse {
                content_items: vec![DynamicToolCallOutputContentItem::InputText {
                    text: "Done.".to_string(),
                }],
                success: true,
                end_turn,
                speak: speak.map(str::to_string),
            },
        })
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_ending_tool_call_ends_the_turn_with_its_speech() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    // Only one response: a follow-up request would find nothing to answer it.
    let responses_mock = responses::mount_sse_once(
        &server,
        sse(vec![
            responses::ev_response_created("resp-1"),
            responses::ev_function_call("jump-call", "jump", "{}"),
            responses::ev_completed("resp-1"),
        ]),
    )
    .await;
    let test = thread_with_tools(&server).await?;

    answer(&test, /*end_turn*/ true, Some("Watch this!")).await?;
    let EventMsg::TurnComplete(complete) = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await
    else {
        unreachable!("event guard guarantees TurnComplete");
    };

    assert_eq!(complete.last_agent_message.as_deref(), Some("Watch this!"));
    assert_eq!(complete.error, None);
    assert_eq!(responses_mock.requests().len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_response_with_another_tool_call_still_gets_its_follow_up() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let responses_mock = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_function_call("jump-call", "jump", "{}"),
                responses::ev_function_call("look-call", "look", "{}"),
                responses::ev_completed("resp-1"),
            ]),
            sse(vec![
                responses::ev_response_created("resp-2"),
                responses::ev_assistant_message("msg-1", "I see a tree."),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let test = thread_with_tools(&server).await?;

    answer(&test, /*end_turn*/ true, Some("Watch this!")).await?;
    answer(&test, /*end_turn*/ false, /*speak*/ None).await?;
    let EventMsg::TurnComplete(complete) = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await
    else {
        unreachable!("event guard guarantees TurnComplete");
    };

    assert_eq!(
        complete.last_agent_message.as_deref(),
        Some("I see a tree.")
    );
    assert_eq!(responses_mock.requests().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_tool_call_beside_it_still_gets_its_follow_up() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let responses_mock = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_function_call("jump-call", "jump", "{}"),
                // No such tool: its error is for the model to see.
                responses::ev_function_call("fly-call", "fly", "{}"),
                responses::ev_completed("resp-1"),
            ]),
            sse(vec![
                responses::ev_response_created("resp-2"),
                responses::ev_assistant_message("msg-1", "I cannot fly."),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let test = thread_with_tools(&server).await?;

    answer(&test, /*end_turn*/ true, Some("Watch this!")).await?;
    let EventMsg::TurnComplete(complete) = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await
    else {
        unreachable!("event guard guarantees TurnComplete");
    };

    assert_eq!(
        complete.last_agent_message.as_deref(),
        Some("I cannot fly.")
    );
    assert_eq!(responses_mock.requests().len(), 2);
    Ok(())
}
