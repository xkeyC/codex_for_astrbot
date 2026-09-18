use codex_code_mode::ImageDetailVisibility;
use codex_code_mode::ToolDefinition as CodeModeToolDefinition;
use codex_tools::FreeformTool;
use codex_tools::FreeformToolFormat;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

/// Fork addition: `exec` as a plain function tool (`{"code": string}`) for
/// Responses-compatible providers that do not support grammar-constrained
/// custom tools. Enabled by `features.code_mode.exec_as_function_tool`.
pub(crate) fn create_code_mode_function_tool(description: String) -> ToolSpec {
    use codex_tools::JsonSchema;
    use codex_tools::ResponsesApiTool;

    let properties = BTreeMap::from([(
        "code".to_string(),
        JsonSchema::string(Some(
            "Raw JavaScript source. It may start with an optional `// @exec: {...}` pragma line."
                .to_string(),
        )),
    )]);
    let description = description.replace(
        "- Accepts raw JavaScript source text, not JSON, quoted strings, or markdown code fences.",
        "- Pass the JavaScript source text in the `code` argument, without markdown code fences.",
    );
    ToolSpec::Function(ResponsesApiTool {
        name: codex_code_mode::PUBLIC_TOOL_NAME.to_string(),
        description,
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["code".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

pub(crate) fn create_code_mode_tool(
    enabled_tools: &[CodeModeToolDefinition],
    deferred_tools: &[CodeModeToolDefinition],
    namespace_descriptions: &BTreeMap<String, codex_code_mode::ToolNamespaceDescription>,
    default_exec_yield_time_ms: u64,
    code_mode_only: bool,
    image_detail_visibility: ImageDetailVisibility,
) -> ToolSpec {
    const CODE_MODE_FREEFORM_GRAMMAR: &str = r#"
start: pragma_source | plain_source
pragma_source: PRAGMA_LINE NEWLINE SOURCE
plain_source: SOURCE

PRAGMA_LINE: /[ \t]*\/\/ @exec:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#;

    ToolSpec::Freeform(FreeformTool {
        name: codex_code_mode::PUBLIC_TOOL_NAME.to_string(),
        description: codex_code_mode::build_exec_tool_description(
            enabled_tools,
            deferred_tools,
            namespace_descriptions,
            default_exec_yield_time_ms,
            code_mode_only,
            image_detail_visibility,
        ),
        defer_loading: None,
        format: FreeformToolFormat {
            r#type: "grammar".to_string(),
            syntax: "lark".to_string(),
            definition: CODE_MODE_FREEFORM_GRAMMAR.to_string(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_tools::ToolName;
    use pretty_assertions::assert_eq;

    #[test]
    fn create_code_mode_tool_matches_expected_spec() {
        let enabled_tools = vec![codex_code_mode::ToolDefinition {
            name: "update_plan".to_string(),
            tool_name: ToolName::plain("update_plan"),
            description: "Update the plan".to_string(),
            kind: codex_code_mode::CodeModeToolKind::Function,
            input_schema: None,
            output_schema: None,
        }];

        assert_eq!(
            create_code_mode_tool(
                &enabled_tools,
                &[],
                &BTreeMap::new(),
                codex_code_mode::DEFAULT_EXEC_YIELD_TIME_MS,
                /*code_mode_only*/ true,
                ImageDetailVisibility::Visible,
            ),
            ToolSpec::Freeform(FreeformTool {
                name: codex_code_mode::PUBLIC_TOOL_NAME.to_string(),
                description: codex_code_mode::build_exec_tool_description(
                    &enabled_tools,
                    &[],
                    &BTreeMap::new(),
                    codex_code_mode::DEFAULT_EXEC_YIELD_TIME_MS,
                    /*code_mode_only*/ true,
                    ImageDetailVisibility::Visible,
                ),
                defer_loading: None,
                format: FreeformToolFormat {
                    r#type: "grammar".to_string(),
                    syntax: "lark".to_string(),
                    definition: r#"
start: pragma_source | plain_source
pragma_source: PRAGMA_LINE NEWLINE SOURCE
plain_source: SOURCE

PRAGMA_LINE: /[ \t]*\/\/ @exec:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#
                    .to_string(),
                },
            })
        );
    }
}
