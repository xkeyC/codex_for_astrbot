use super::*;
use crate::CodeModeToolKind;
use codex_protocol::ToolName;
use pretty_assertions::assert_eq;
use serde_json::json;

fn options() -> RunCodeDescriptionOptions {
    RunCodeDescriptionOptions {
        default_exec_yield_time_ms: 10_000,
        image_detail_visibility: ImageDetailVisibility::Hidden,
    }
}

fn lookup() -> ToolDefinition {
    ToolDefinition {
        name: "astrbot__lookup".to_string(),
        tool_name: ToolName::namespaced("astrbot__", "lookup"),
        description: "Looks a key up.\nReturns its value.".to_string(),
        kind: CodeModeToolKind::Function,
        input_schema: Some(json!({
            "type": "object",
            "properties": { "key": { "type": "string" } },
            "required": ["key"],
            "additionalProperties": false
        })),
        input_schema_max_bytes: None,
        output_schema: None,
    }
}

fn apply_patch() -> ToolDefinition {
    ToolDefinition {
        name: "apply_patch".to_string(),
        tool_name: ToolName::plain("apply_patch"),
        description: "Applies a patch.".to_string(),
        kind: CodeModeToolKind::Freeform,
        input_schema: None,
        input_schema_max_bytes: None,
        output_schema: None,
    }
}

#[test]
fn declares_tools_sorted_in_one_block() {
    let namespaces = BTreeMap::from([(
        "astrbot__".to_string(),
        ToolNamespaceDescription {
            name: "astrbot__".to_string(),
            description: "Host tools.".to_string(),
        },
    )]);
    let description =
        build_run_code_tool_description(&[lookup(), apply_patch()], &namespaces, options());
    // The same tools in another order give the same bytes.
    assert_eq!(
        description,
        build_run_code_tool_description(&[apply_patch(), lookup()], &namespaces, options())
    );
    assert!(description.starts_with(RUN_CODE_INTRO));
    assert!(description.contains("## Writing code for run_code"));
    assert!(description.contains(DEFERRED_TOOLS_CATALOG));
    assert!(description.contains("after 10000 ms"));
    // No tool returns an MCP result: no MCP types.
    assert!(!description.contains("type CallToolResult"));
    let block = description
        .split_once("Program-only SDK bindings:\n```ts\n")
        .and_then(|(_, block)| block.strip_suffix("\n```"))
        .expect("the declarations block ends the description");
    assert!(block.starts_with("type JsonValue ="));
    assert!(block.contains("declare function inspect(...values: unknown[]): void;"));
    assert!(block.contains("declare function exit(): never;"));
    let tools_part = &block[block.find("interface ToolArgsMap").expect("args map")..];
    assert_eq!(
        tools_part,
        r#"interface ToolArgsMap {
  /** Applies a patch. */
  "apply_patch": string;
  // Host tools.
  /**
   * Looks a key up.
   * Returns its value.
   */
  "astrbot__lookup": { key: string; };
}
interface ToolOutputMap {
  "apply_patch": unknown;
  "astrbot__lookup": unknown;
}
type ToolName = keyof ToolArgsMap;
declare const tools: { [K in ToolName]: (args: ToolArgsMap[K]) => Promise<ToolOutputMap[K]> };"#
    );
}

#[test]
fn mentions_tool_search_and_mcp_types_only_when_used() {
    let search = ToolDefinition {
        name: "tool_search".to_string(),
        tool_name: ToolName::plain("tool_search"),
        description: "upstream guidance".to_string(),
        kind: CodeModeToolKind::Function,
        input_schema: Some(json!({
            "type": "object",
            "properties": { "query": { "type": "string" }, "limit": { "type": "number" } },
            "required": ["query"]
        })),
        input_schema_max_bytes: None,
        output_schema: None,
    };
    let mcp = ToolDefinition {
        output_schema: Some(json!({
            "type": "object",
            "properties": {
                "content": { "type": "array", "items": { "type": "object" } },
                "isError": { "type": "boolean" },
                "_meta": { "type": "object" }
            }
        })),
        ..lookup()
    };
    let description = build_run_code_tool_description(&[search, mcp], &BTreeMap::new(), options());
    assert!(description.contains(DEFERRED_TOOLS_SEARCH));
    assert!(description.contains(RUN_CODE_TOOL_SEARCH_DESCRIPTION));
    assert!(!description.contains("upstream guidance"));
    assert!(description.contains("type CallToolResult"));
    assert!(description.contains("\"astrbot__lookup\": CallToolResult;"));
}

#[test]
fn renders_one_tool_like_the_sdk_block() {
    assert_eq!(
        render_run_code_tool_declaration(&ToolDefinition {
            description: "Looks a key */ up.".to_string(),
            ..lookup()
        }),
        r#"interface ToolArgsMap {
  /** Looks a key *\/ up. */
  "astrbot__lookup": { key: string; };
}
interface ToolOutputMap {
  "astrbot__lookup": unknown;
}"#
    );
}
