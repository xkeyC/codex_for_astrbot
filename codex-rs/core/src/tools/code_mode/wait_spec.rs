use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub(crate) fn create_wait_tool(
    description_override: Option<&str>,
    parameters_override: Option<&str>,
) -> ToolSpec {
    create_wait_tool_for(description_override, parameters_override, "exec cell")
}

/// Fork addition: `wait` for the `run_code` programs of open-weight models.
pub(crate) fn create_run_code_wait_tool(compact: bool) -> ToolSpec {
    let description = codex_code_mode::build_run_code_wait_tool_description(compact);
    if compact {
        let mut spec = create_compact_wait_tool();
        if let ToolSpec::Function(tool) = &mut spec {
            tool.description = description.to_string();
        }
        spec
    } else {
        create_wait_tool_for(
            Some(description),
            /*parameters_override*/ None,
            "run_code program",
        )
    }
}

fn create_wait_tool_for(
    description_override: Option<&str>,
    parameters_override: Option<&str>,
    cell: &str,
) -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "cell_id".to_string(),
            JsonSchema::string(Some(format!("Identifier of the running {cell}."))),
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
            JsonSchema::boolean(Some(format!(
                "True stops the running {cell}; false or omitted waits for output."
            ))),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: codex_code_mode::WAIT_TOOL_NAME.to_string(),
        description: description_override.map(str::to_owned).unwrap_or_else(|| {
            format!(
                "Waits on a yielded `{}` cell and returns new output or completion.\n{}",
                codex_code_mode::PUBLIC_TOOL_NAME,
                codex_code_mode::build_wait_tool_description().trim()
            )
        }),
        strict: false,
        parameters: parameters_override
            .and_then(
                |parameters| match codex_tools::parse_catalog_tool_parameters(parameters) {
                    Ok(parameters) => Some(parameters),
                    Err(reason) => {
                        tracing::warn!(
                            tool = codex_code_mode::WAIT_TOOL_NAME,
                            reason,
                            "Invalid catalog tool parameters; using bundled parameters"
                        );
                        None
                    }
                },
            )
            .unwrap_or_else(|| {
                JsonSchema::object(
                    properties,
                    Some(vec!["cell_id".to_string()]),
                    Some(false.into()),
                )
            }),
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
            "Resume a `{}` cell that returned `Script running with cell ID ...`: returns output since the last yield, or the final result. `terminate: true` stops the cell. Only cell IDs: a shell session ID from `exec_command` is polled with `write_stdin`.",
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
            create_wait_tool(/*description_override*/ None, /*parameters_override*/ None),
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
