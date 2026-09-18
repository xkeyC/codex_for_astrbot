//! Dynamic tools can be replaced on a live thread without losing history.

use anyhow::Result;
use codex_core::StartThreadOptions;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceTool;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::mcp::ClientMcpExtensions;
use codex_protocol::protocol::ThreadSettingsOverrides;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::submit_thread_settings;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;

fn namespace_with(tools: &[&str]) -> DynamicToolSpec {
    DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
        name: "astrbot".to_string(),
        description: "Host tools.".to_string(),
        tools: tools
            .iter()
            .map(|name| {
                DynamicToolNamespaceTool::Function(DynamicToolFunctionSpec {
                    name: (*name).to_string(),
                    description: format!("The {name} tool."),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {},
                        "additionalProperties": false,
                    }),
                    defer_loading: false,
                })
            })
            .collect(),
    })
}

/// Names of every function tool in a request, including namespaced ones.
fn tool_names(body: &Value) -> Vec<String> {
    fn visit(tool: &Value, out: &mut Vec<String>) {
        match tool.get("type").and_then(Value::as_str) {
            Some("namespace") => {
                for sub in tool
                    .get("tools")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    visit(sub, out);
                }
            }
            _ => {
                if let Some(name) = tool.get("name").and_then(Value::as_str) {
                    out.push(name.to_string());
                }
            }
        }
    }
    let mut out = Vec::new();
    for tool in body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        visit(tool, &mut out);
    }
    out
}

#[tokio::test]
async fn replacing_dynamic_tools_keeps_history_and_persists() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let base_test = test_codex().build_with_auto_env(&server).await?;
    let new_thread = base_test
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: vec![namespace_with(&["tool_alpha"])],
            ..StartThreadOptions::new(base_test.config.clone())
        })
        .await?;
    let mut test = base_test;
    test.codex = new_thread.thread;
    test.session_configured = new_thread.session_configured;

    let first = responses::mount_sse_once(&server, responses::sse_completed("first")).await;
    test.submit_text_turn("remember the word PINEAPPLE").await?;
    let first_body = first.single_request().body_json();
    let names = tool_names(&first_body);
    assert!(names.iter().any(|n| n == "tool_alpha"), "{names:?}");

    submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            dynamic_tools: Some(vec![namespace_with(&["tool_beta"])]),
            ..Default::default()
        },
    )
    .await?;
    let snapshot = test.codex.thread_settings_snapshot().await;
    assert_eq!(
        snapshot.dynamic_tools,
        Some(vec![namespace_with(&["tool_beta"])])
    );

    let second = responses::mount_sse_once(&server, responses::sse_completed("second")).await;
    test.submit_text_turn("what was the word?").await?;
    let second_body = second.single_request().body_json();
    let names = tool_names(&second_body);
    assert!(names.iter().any(|n| n == "tool_beta"), "{names:?}");
    assert!(!names.iter().any(|n| n == "tool_alpha"), "{names:?}");
    assert!(
        second_body.to_string().contains("PINEAPPLE"),
        "history from the first turn must survive the tool swap"
    );

    // Cold resume: the replaced set, not the start-time session meta, is restored.
    test.codex.flush_rollout().await?;
    test.codex.shutdown_and_wait().await?;
    let rollout_path = test.codex.rollout_path().expect("rollout path");
    let resumed = test
        .thread_manager
        .resume_thread_from_rollout(
            test.config.clone(),
            rollout_path,
            test.thread_manager.auth_manager(),
            /*parent_trace*/ None,
            ClientMcpExtensions::default(),
        )
        .await?
        .thread;
    let third = responses::mount_sse_once(&server, responses::sse_completed("third")).await;
    resumed
        .start_or_steer_turn(codex_core::TurnInputRequest::user_input(vec![
            codex_protocol::user_input::UserInput::Text {
                text: "after resume".to_string(),
                text_elements: Vec::new(),
            },
        ]))
        .await?;
    core_test_support::wait_for_event(&resumed, |event| {
        matches!(event, codex_protocol::protocol::EventMsg::TurnComplete(_))
    })
    .await;
    let third_body = third.single_request().body_json();
    let names = tool_names(&third_body);
    assert!(names.iter().any(|n| n == "tool_beta"), "{names:?}");
    assert!(!names.iter().any(|n| n == "tool_alpha"), "{names:?}");
    resumed.shutdown_and_wait().await?;
    Ok(())
}
