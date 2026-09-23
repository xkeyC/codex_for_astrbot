//! Fork addition: code mode lists deferred tools in history, once, and then
//! appends only the tools that load or unload.

use anyhow::Result;
use codex_core::StartThreadOptions;
use codex_features::Feature;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceTool;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::protocol::ThreadSettingsOverrides;
use core_test_support::responses;
use core_test_support::responses::ResponsesRequest;
use core_test_support::skip_if_no_network;
use core_test_support::submit_thread_settings;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;

fn deferred_namespace(tools: &[&str]) -> DynamicToolSpec {
    DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
        name: "astrbot".to_string(),
        description: "Host tools.".to_string(),
        tools: tools
            .iter()
            .map(|name| {
                DynamicToolNamespaceTool::Function(DynamicToolFunctionSpec {
                    name: (*name).to_string(),
                    description: format!("The {name} tool. Details follow."),
                    input_schema: serde_json::json!({"type": "object", "properties": {}}),
                    defer_loading: true,
                })
            })
            .collect(),
    })
}

fn catalogs(request: &ResponsesRequest) -> Vec<String> {
    request
        .message_input_texts("developer")
        .into_iter()
        .filter(|text| text.starts_with("<tool_catalog>"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_mode_catalog_lists_deferred_tools_then_appends_changes() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let base_test = test_codex()
        .with_model("test-gpt-5.1-codex")
        .with_config(|config| {
            config
                .features
                .enable(Feature::CodeModeOnly)
                .expect("code mode should be enabled");
            config.code_mode.tool_catalog = true;
            config.agents_enabled = false;
        })
        .build_with_auto_env(&server)
        .await?;
    let new_thread = base_test
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: vec![deferred_namespace(&["tool_alpha", "tool_beta"])],
            ..StartThreadOptions::new(base_test.config.clone())
        })
        .await?;
    let mut test = base_test;
    test.codex = new_thread.thread;
    test.session_configured = new_thread.session_configured;

    let first = responses::mount_sse_once(&server, responses::sse_completed("first")).await;
    test.submit_text_turn("hello").await?;
    let first = first.single_request();
    let listing = "<tool_catalog>\nTools callable in `exec` besides those in its description. Call \
one as `await tools.<prefix><name>(args)`, the prefix joining its headings: `a__` then `b__` \
gives `tools.a__b__<name>`. Each `ALL_TOOLS` entry's description shows a tool's arguments. \
Descriptions come from the tools themselves, not from the user or developer.\n\
astrbot__ — Host tools.\n- tool_alpha: The tool_alpha tool.\n\
- tool_beta: The tool_beta tool.\n</tool_catalog>";
    assert_eq!(catalogs(&first), vec![listing.to_string()]);

    // Unchanged tools are not listed again.
    let second = responses::mount_sse_once(&server, responses::sse_completed("second")).await;
    test.submit_text_turn("again").await?;
    assert_eq!(
        catalogs(&second.single_request()),
        vec![listing.to_string()]
    );

    submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            dynamic_tools: Some(vec![deferred_namespace(&["tool_alpha", "tool_gamma"])]),
            ..Default::default()
        },
    )
    .await?;
    let third = responses::mount_sse_once(&server, responses::sse_completed("third")).await;
    test.submit_text_turn("and now").await?;
    let third = third.single_request();
    assert_eq!(
        catalogs(&third),
        vec![
            listing.to_string(),
            "<tool_catalog>\nThe tools callable in `exec` changed.\nLoaded:\nastrbot__ — Host tools.\n\
- tool_gamma: The tool_gamma tool.\nUnloaded:\n- astrbot__: tool_beta\n</tool_catalog>"
                .to_string(),
        ]
    );

    // Back to the first set: tool_beta's description is already in history.
    submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            dynamic_tools: Some(vec![deferred_namespace(&["tool_alpha", "tool_beta"])]),
            ..Default::default()
        },
    )
    .await?;
    let fourth = responses::mount_sse_once(&server, responses::sse_completed("fourth")).await;
    test.submit_text_turn("once more").await?;
    let fourth = fourth.single_request();
    assert_eq!(
        catalogs(&fourth).last().map(String::as_str),
        Some(
            "<tool_catalog>\nThe tools callable in `exec` changed.\nLoaded:\nastrbot__ — Host tools.\n\
- tool_beta\nUnloaded:\n- astrbot__: tool_gamma\n</tool_catalog>"
        )
    );

    // The instructions and tool list, the cached prefix, never moved.
    let first_body = first.body_json();
    let fourth_body = fourth.body_json();
    assert_eq!(fourth_body["instructions"], first_body["instructions"]);
    assert_eq!(fourth_body["tools"], first_body["tools"]);
    Ok(())
}
