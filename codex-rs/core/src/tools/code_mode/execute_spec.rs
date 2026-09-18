use codex_code_mode::ImageDetailVisibility;
use codex_code_mode::ToolDefinition as CodeModeToolDefinition;
use codex_tools::FreeformTool;
use codex_tools::FreeformToolFormat;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

/// Fork addition: a much shorter replacement for the generic part of the
/// `exec` description. Tool declarations and deferred-tool guidance that
/// follow it are kept. Enabled by `features.code_mode.compact_exec_description`.
pub(crate) const COMPACT_EXEC_DESCRIPTION: &str = r#"Run JavaScript to call tools and compose their results (async module in a fresh V8 isolate; no Node, file system, network or console).
- Tools are async functions on the global `tools` object, e.g. `await tools.ns__name({...})`. `ALL_TOOLS` lists `{ name, description }` for every tool, including ones not described here.
- Output: `text(value)`, `image(itemOrDataUrl)`, `notify(value)` (sent immediately), `store(key, value)` / `load(key)` (kept across exec calls), `exit()`.
- Optional first line: `// @exec: {"yield_time_ms": 10000, "max_output_tokens": 10000}`. A script still running when it yields continues via `wait`.
- Input is raw JavaScript source, not JSON or markdown fences."#;

/// Replaces the generic template at the start of `description` with the compact text.
pub(crate) fn compact_exec_description(
    description: &str,
    default_exec_yield_time_ms: u64,
    image_detail_visibility: ImageDetailVisibility,
) -> String {
    let template = codex_code_mode::build_exec_tool_description(
        &[],
        &[],
        &BTreeMap::new(),
        default_exec_yield_time_ms,
        /*code_mode_only*/ false,
        image_detail_visibility,
    );
    match description.strip_prefix(template.as_str()) {
        Some(rest) => format!("{COMPACT_EXEC_DESCRIPTION}{rest}"),
        None => description.to_string(),
    }
}

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

#[cfg(test)]
mod compact_description_tests {
    use super::*;

    #[test]
    fn compact_description_keeps_tool_sections() {
        let visibility = ImageDetailVisibility::Visible;
        let deferred = vec![codex_code_mode::ToolDefinition {
            name: "astrbot__x".to_string(),
            tool_name: codex_tools::ToolName::plain("astrbot__x"),
            description: "x".to_string(),
            kind: codex_code_mode::CodeModeToolKind::Function,
            input_schema: None,
            output_schema: None,
        }];
        let ToolSpec::Freeform(full) = create_code_mode_tool(
            &[],
            &deferred,
            &BTreeMap::new(),
            codex_code_mode::DEFAULT_EXEC_YIELD_TIME_MS,
            /*code_mode_only*/ true,
            visibility,
        ) else {
            unreachable!("exec is a freeform tool");
        };
        let compact = compact_exec_description(
            &full.description,
            codex_code_mode::DEFAULT_EXEC_YIELD_TIME_MS,
            visibility,
        );
        assert!(compact.starts_with(COMPACT_EXEC_DESCRIPTION));
        assert!(compact.contains("ALL_TOOLS"));
        assert!(
            compact.len() < full.description.len() / 2,
            "{} vs {}",
            compact.len(),
            full.description.len()
        );
    }
}
