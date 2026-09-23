use std::sync::Arc;

use crate::session::step_context::StepContext;
use crate::session::tests::make_session_and_context;
use crate::tools::spec_plan::append_source_tools;
use crate::tools::spec_plan::build_core_tool_registry;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceTool;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::CatalogTool;
use super::deferred_code_mode_tools;

fn function(name: &str, description: &str, defer_loading: bool) -> DynamicToolFunctionSpec {
    DynamicToolFunctionSpec {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: json!({"type": "object", "properties": {}}),
        defer_loading,
    }
}

fn namespace(
    name: &str,
    description: &str,
    tools: Vec<DynamicToolFunctionSpec>,
) -> DynamicToolSpec {
    DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
        name: name.to_string(),
        description: description.to_string(),
        tools: tools
            .into_iter()
            .map(DynamicToolNamespaceTool::Function)
            .collect(),
    })
}

#[tokio::test]
async fn lists_deferred_tools_by_their_code_mode_names() {
    let (_, turn) = make_session_and_context().await;
    let turn = Arc::new(turn);
    let step_context = StepContext::for_test(Arc::clone(&turn));
    let dynamic_tools = vec![
        namespace(
            "astrbot",
            "AstrBot plugin tools.",
            vec![
                function("web_search", "Search the web.", /*defer_loading*/ true),
                function("send-file", "Send a file.", /*defer_loading*/ true),
                function(
                    "always_there",
                    "Listed in exec.",
                    /*defer_loading*/ false,
                ),
            ],
        ),
        namespace(
            "hidden",
            "Excluded from code mode.",
            vec![function(
                "secret",
                "Not callable.",
                /*defer_loading*/ true,
            )],
        ),
        DynamicToolSpec::Function(function(
            "lookup",
            "Look it up.",
            /*defer_loading*/ true,
        )),
    ];
    let mut registry = build_core_tool_registry(
        step_context.turn.as_ref(),
        step_context.turn.model_info(),
        &step_context.environments,
        step_context.mcp.as_ref(),
        /*tool_suggest_candidates*/ None,
        /*wait_for_environment_tool_config*/ None,
    );
    append_source_tools(
        step_context.turn.as_ref(),
        step_context.turn.model_info(),
        &mut registry,
        Vec::new(),
        Vec::new(),
        &dynamic_tools,
    );

    let mut tools = deferred_code_mode_tools(&registry, &["hidden".to_string()])
        .into_iter()
        .filter(|tool| tool.group == "astrbot__" || tool.global_name == "lookup")
        .collect::<Vec<_>>();
    tools.sort_by(|left, right| left.global_name.cmp(&right.global_name));

    assert_eq!(
        tools,
        vec![
            CatalogTool {
                global_name: "astrbot__send_file".to_string(),
                group: "astrbot__".to_string(),
                description: "Send a file.".to_string(),
                group_description: "AstrBot plugin tools.".to_string(),
            },
            CatalogTool {
                global_name: "astrbot__web_search".to_string(),
                group: "astrbot__".to_string(),
                description: "Search the web.".to_string(),
                group_description: "AstrBot plugin tools.".to_string(),
            },
            CatalogTool {
                global_name: "lookup".to_string(),
                group: String::new(),
                description: "Look it up.".to_string(),
                group_description: String::new(),
            },
        ]
    );
    assert!(
        deferred_code_mode_tools(&registry, &["hidden".to_string()])
            .iter()
            .all(|tool| !tool.global_name.starts_with("hidden"))
    );
}
