use super::*;
use crate::CodeModeToolKind;
use codex_protocol::ToolName;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeSet;

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

#[test]
fn says_the_program_is_typescript_and_declares_generated_image() {
    let description = build_run_code_tool_description(&[lookup()], &BTreeMap::new(), options());
    assert!(description.starts_with("Run a TypeScript program against the available tools."));
    assert!(description.contains("type-checked against the declarations below before it runs"));
    assert!(description.contains("no `enum`, `namespace`, parameter properties or decorators"));
    assert!(description.contains("`console` (show values with `inspect`)"));
    assert!(!description.contains("JavaScript"));
    assert!(description.contains(
        "declare function generatedImage(result: { image_url: string; output_hint?: string }): void;"
    ));
    assert!(description.contains("declare function load(key: string): unknown;"));
    // The checker's extra globals stay out of the prompt.
    assert!(!description.contains("declare function setTimeout"));
}

#[test]
fn checker_declarations_accept_more_than_the_prompt_and_drop_comments() {
    let typed = ToolDefinition {
        name: "typed".to_string(),
        tool_name: ToolName::plain("typed"),
        output_schema: Some(
            json!({ "type": "object", "properties": { "n": { "type": "number" } } }),
        ),
        ..lookup()
    };
    let declarations = build_run_code_checker_declarations(
        &[lookup(), apply_patch(), typed.clone()],
        &BTreeSet::new(),
    );
    let text = &declarations.text;
    assert!(text.contains("declare function load(key: string): any;"));
    assert!(text.contains("declare function setTimeout("));
    assert!(text.contains("declare function yield_control(): void;"));
    assert!(text.contains("declare function content(value: unknown): void;"));
    assert!(text.contains("declare function generatedImage("));
    // The image declaration accepts `detail` whatever the prompt shows.
    assert!(text.contains("detail?: \"auto\" | \"low\" | \"high\" | \"original\"): void;"));
    assert!(!text.contains("Looks a key up"));
    let tools_part = &text[text.find("interface ToolArgsMap").expect("args map")..];
    assert_eq!(
        tools_part,
        r#"interface ToolArgsMap {
  "apply_patch": string;
  "astrbot__lookup": { key: string; };
  "typed": { key: string; };
}
interface ToolOutputMap {
  "apply_patch": any;
  "astrbot__lookup": any;
  "typed": { n?: number; };
}
type ToolName = keyof ToolArgsMap;
declare const tools: { [K in ToolName]: (args: ToolArgsMap[K]) => Promise<ToolOutputMap[K]> };"#
    );

    // Each tool's entries are found by line, and an untyped tool is `any`.
    let lines = text.lines().collect::<Vec<_>>();
    let line_of = |needle: &str| {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .expect("the line")
    };
    assert_eq!(
        declarations.tools_at_lines(&[line_of("\"typed\": { n?")]),
        BTreeSet::from(["typed".to_string()])
    );
    assert_eq!(
        declarations.tools_at_lines(&[line_of("\"astrbot__lookup\": { key"), 0]),
        BTreeSet::from(["astrbot__lookup".to_string()])
    );
    let untyped = build_run_code_checker_declarations(
        &[lookup(), typed],
        &BTreeSet::from(["typed".to_string()]),
    );
    assert!(
        untyped
            .text
            .contains("  \"typed\": any;\n}\ninterface ToolOutputMap")
    );
    assert!(untyped.text.contains("  \"typed\": any;\n}\ntype ToolName"));
}

#[test]
fn checker_declarations_track_multi_line_entries() {
    let described = ToolDefinition {
        name: "described".to_string(),
        tool_name: ToolName::plain("described"),
        input_schema: Some(json!({
            "type": "object",
            "properties": { "a": { "type": "string", "description": "First.\nSecond." } }
        })),
        ..lookup()
    };
    let declarations =
        build_run_code_checker_declarations(&[described, lookup()], &BTreeSet::new());
    let lines = declarations.text.lines().collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| line.starts_with("  \"described\": {"))
        .expect("the entry");
    for (line, text) in lines.iter().enumerate().skip(start).take(5) {
        assert_eq!(
            declarations.tools_at_lines(&[line]),
            BTreeSet::from(["described".to_string()]),
            "{text}"
        );
    }
    assert_eq!(
        declarations.tools_at_lines(&[start - 1]),
        BTreeSet::from(["astrbot__lookup".to_string()])
    );
    assert_eq!(lines[start + 5], "}");
    assert_eq!(declarations.tools_at_lines(&[start + 5]), BTreeSet::new());
}
