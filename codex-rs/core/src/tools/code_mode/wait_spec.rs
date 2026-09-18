use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub(crate) fn create_wait_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "cell_id".to_string(),
            JsonSchema::string(Some("Identifier of the running exec cell.".to_string())),
        ),
        (
            "yield_time_ms".to_string(),
            JsonSchema::number(Some(
                "Wait before yielding more output. Defaults to 10000 ms.".to_string(),
            )),
        ),
        (
            "max_tokens".to_string(),
            JsonSchema::number(Some(
                "Output token budget for this wait call. Defaults to 10000 tokens.".to_string(),
            )),
        ),
        (
            "terminate".to_string(),
            JsonSchema::boolean(Some(
                "True stops the running exec cell; false or omitted waits for output.".to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: codex_code_mode::WAIT_TOOL_NAME.to_string(),
        description: format!(
            "Waits on a yielded `{}` cell and returns new output or completion.\n{}",
            codex_code_mode::PUBLIC_TOOL_NAME,
            codex_code_mode::build_wait_tool_description().trim()
        ),
        strict: false,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["cell_id".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
        defer_loading: None,
    })
}

/// Fork addition: a compact `wait` tool, used with
/// `features.code_mode.compact_exec_description`.
pub(crate) fn create_compact_wait_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "cell_id".to_string(),
            JsonSchema::string(/*description*/ None),
        ),
        (
            "yield_time_ms".to_string(),
            JsonSchema::number(/*description*/ None),
        ),
        (
            "max_tokens".to_string(),
            JsonSchema::number(/*description*/ None),
        ),
        (
            "terminate".to_string(),
            JsonSchema::boolean(/*description*/ None),
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: codex_code_mode::WAIT_TOOL_NAME.to_string(),
        description: format!(
            "Resume a `{}` cell that returned `Script running with cell ID ...`: returns output since the last yield, or the final result. `terminate: true` stops the cell.",
            codex_code_mode::PUBLIC_TOOL_NAME
        ),
        strict: false,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["cell_id".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
        defer_loading: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn create_wait_tool_matches_expected_spec() {
        assert_eq!(
            create_wait_tool(),
            ToolSpec::Function(ResponsesApiTool {
                name: codex_code_mode::WAIT_TOOL_NAME.to_string(),
                description: format!(
                    "Waits on a yielded `{}` cell and returns new output or completion.\n{}",
                    codex_code_mode::PUBLIC_TOOL_NAME,
                    codex_code_mode::build_wait_tool_description().trim()
                ),
                strict: false,
                defer_loading: None,
                parameters: JsonSchema::object(
                    BTreeMap::from([
                        (
                            "cell_id".to_string(),
                            JsonSchema::string(Some(
                                "Identifier of the running exec cell.".to_string()
                            )),
                        ),
                        (
                            "max_tokens".to_string(),
                            JsonSchema::number(Some(
                                "Output token budget for this wait call. Defaults to 10000 tokens."
                                    .to_string(),
                            )),
                        ),
                        (
                            "terminate".to_string(),
                            JsonSchema::boolean(Some(
                                "True stops the running exec cell; false or omitted waits for output."
                                    .to_string(),
                            )),
                        ),
                        (
                            "yield_time_ms".to_string(),
                            JsonSchema::number(Some(
                                "Wait before yielding more output. Defaults to 10000 ms."
                                    .to_string(),
                            )),
                        ),
                    ]),
                    Some(vec!["cell_id".to_string()]),
                    Some(false.into()),
                ),
                output_schema: None,
            })
        );
    }
}
