//! Fork addition: Code Mode presented to open-weight models as a
//! `run_code({code, description})` function tool.
//!
//! The runtime, its cells and their async semantics are Code Mode's own; only
//! the tool's name, arguments and description differ. The description is a
//! guide to writing the program and one consolidated TypeScript block that
//! declares the runtime's globals and every declared tool (sorted by name, so
//! its bytes stay the same for the prompt cache).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::ImageDetailVisibility;
use crate::ToolDefinition;
use crate::ToolNamespaceDescription;
use crate::description::MCP_TYPESCRIPT_PREAMBLE;
use crate::description::definition_input_type;
use crate::description::definition_output_type;
use crate::normalize_code_mode_identifier;

/// The name open models call Code Mode's `exec` by.
pub const RUN_CODE_TOOL_NAME: &str = "run_code";

/// The nested search tool's description in the SDK declarations.
pub const RUN_CODE_TOOL_SEARCH_DESCRIPTION: &str = "Finds tools that are not declared here (BM25 over their names and descriptions). Returns up to `limit` matches (8 by default) as `{ tools: [{ name, description }] }`; each `description` is the tool's TypeScript declaration.";

const TOOL_SEARCH_NAME: &str = "tool_search";

const RUN_CODE_INTRO: &str = "Run a JavaScript program against the available tools. Takes two required arguments: `code`, the program to run, and `description`, a short active-voice summary of what the program does (for example \"Look up the weather in Paris\"), shown as its label. Call tools as `await tools.name(args)` per the declarations below, and show what you need with `inspect(...values)`.";

const RUN_CODE_GUIDE: &str = r#"## Writing code for run_code

`run_code` takes two required arguments: `code`, the JavaScript source of the program (no markdown fences), and `description`, a short summary of what the program does. The program runs as an async module (top-level `await` works) in a fresh V8 isolate: plain JavaScript, with no Node APIs, file system, network or console. The declarations below are SDK bindings for this program. A declaration does not make its name a directly callable tool; only names supplied as separate tool schemas may be called directly.

Inside the program:

- Call tools as `await tools.name(args)` — quoted access for exotic names: `tools["my-tool"](args)`. Tool arguments must be lossless JSON.
- A FAILED tool call rejects with its error message as a plain string: `try/catch` it to handle the failure and continue.
- Independent read-only calls MAY overlap under `Promise.all`; sequence dependent work with `await`.
- Show selected values and images with `inspect(...values)` (`text(value)` and `image(...)` append one item each); every other intermediate result stays out of the conversation, so extract just what you need. `notify(value)` shows a value at once, while the program keeps running.
- `store(key, value)` keeps a JSON value for later `run_code` calls and `load(key)` reads it back; nothing else outlives the program. `exit()` ends it early, successfully; promises still pending when it ends are dropped.
- {{ deferred_tools }}

Long-running programs are asynchronous. `run_code` returns when the program finishes or after {{ yield_time_ms }} ms, whichever comes first. A program still running then keeps running, its tool calls included: the result says `Script running with cell ID <id>` and holds the output so far, you get control back, and you collect what follows with `wait({ cell_id })` (again while it is still running; `terminate: true` stops it). An optional first line `// @exec: {"yield_time_ms": 10000, "max_output_tokens": 10000}` sets how long `run_code` waits before it returns and its output token budget (10000 tokens by default)."#;

const DEFERRED_TOOLS_SEARCH: &str = "Tools not declared below may exist (`ALL_TOOLS` lists every tool the program can call). Find them with `await tools.tool_search({ query: \"...\", limit: 8 })`: it returns `{ tools: [{ name, description }] }`, each `description` the TypeScript declaration of the tool's arguments and result. `inspect` the matches, then call `tools[name](args)`.";

const DEFERRED_TOOLS_CATALOG: &str = "Tools not declared below may exist: `ALL_TOOLS` lists every tool the program can call as `{ name, description }`, each description with the tool's TypeScript declaration. Filter it by `name` and `description` to find one, `inspect` it, then call `tools[name](args)`.";

const RUNTIME_DECLARATIONS_HEAD: &str = r#"type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
/** Shows values in the conversation: strings as text, images (a `data:` URL, `{ image_url }` or an MCP image block) as images, anything else as JSON. */
declare function inspect(...values: unknown[]): void;
/** Appends a text item; a value other than a string is JSON-stringified. */
declare function text(value: unknown): void;"#;

const IMAGE_DECLARATION_WITH_DETAIL: &str = r#"/** Appends an image: a base64 `data:` URL, `{ image_url }`, or an MCP image block of a tool result. `detail` overrides the detail the item asks for. */
declare function image(imageUrlOrItem: string | { image_url: string; detail?: "auto" | "low" | "high" | "original" } | { type: "image"; data: string; mimeType: string }, detail?: "auto" | "low" | "high" | "original"): void;"#;

const IMAGE_DECLARATION: &str = r#"/** Appends an image: a base64 `data:` URL, `{ image_url }`, or an MCP image block of a tool result. */
declare function image(imageUrlOrItem: string | { image_url: string } | { type: "image"; data: string; mimeType: string }): void;"#;

const RUNTIME_DECLARATIONS_TAIL: &str = r#"/** Sends a text update to the conversation right away, while the program keeps running; a value other than a string is JSON-stringified. */
declare function notify(value: unknown): void;
/** Keeps a JSON-serializable value under `key` for later run_code calls of this conversation. */
declare function store(key: string, value: unknown): void;
/** The value an earlier run_code call stored under `key`, or undefined. */
declare function load(key: string): unknown;
/** Ends the program successfully, right away. */
declare function exit(): never;
/** Every tool the program can call, declared here or not, with its description. */
declare const ALL_TOOLS: ReadonlyArray<{ name: string; description: string }>;"#;

const RUN_CODE_WAIT_DESCRIPTION: &str = r#"Waits on a `run_code` program that is still running, and returns its new output or its result.
- Use `wait` only after `run_code` returned `Script running with cell ID ...`: that program kept running (its tool calls too), and `wait` collects what it produced since.
- `cell_id` is that ID.
- `yield_time_ms` controls how long to wait for more output before yielding again. Defaults to 10000 ms.
- `max_tokens` limits how much new output this wait call returns. Defaults to 10000 tokens.
- `terminate: true` stops the program; false or omitted waits for output.
- `wait` returns only the new output since the last yield, or the program's final result.
- If the program is still running, `wait` may yield again with the same `cell_id`: call it again.
- Once the program has finished, `wait` returns its result and the cell closes.
- `wait` takes only cell IDs from `Script running with cell ID ...`. IDs that other tools report go back to those tools: a shell `session ID` from `exec_command` is polled with `write_stdin`, a task id with the tool that named it."#;

const RUN_CODE_COMPACT_WAIT_DESCRIPTION: &str = "Resume a `run_code` program that returned `Script running with cell ID ...` (it kept running): returns its output since the last yield, or its final result. `terminate: true` stops it. Only cell IDs: a shell session ID from `exec_command` is polled with `write_stdin`.";

/// How the runtime is set up for the described programs.
#[derive(Clone, Copy, Debug)]
pub struct RunCodeDescriptionOptions {
    pub default_exec_yield_time_ms: u64,
    pub image_detail_visibility: ImageDetailVisibility,
}

/// The `run_code` description: the guide, then the SDK declarations of the
/// runtime's globals and of `enabled_tools` (the nested tools declared up
/// front; a `tool_search` among them is how the others are found).
pub fn build_run_code_tool_description(
    enabled_tools: &[ToolDefinition],
    namespace_descriptions: &BTreeMap<String, ToolNamespaceDescription>,
    options: RunCodeDescriptionOptions,
) -> String {
    let mut tools = enabled_tools
        .iter()
        .map(|tool| (normalize_code_mode_identifier(&tool.name), tool))
        .collect::<Vec<_>>();
    tools.sort_by(|(left, _), (right, _)| left.cmp(right));
    tools.dedup_by(|(left, _), (right, _)| left == right);

    // Code mode's own search tool, by the name it has here (`codex_tool_search`
    // where a host tool took `tool_search`).
    let search_name = tools
        .iter()
        .find(|(_, tool)| is_search_tool(tool))
        .map(|(name, _)| name.clone());
    let deferred_tools = match &search_name {
        Some(name) => DEFERRED_TOOLS_SEARCH.replace(
            &format!("tools.{TOOL_SEARCH_NAME}("),
            &format!("tools.{name}("),
        ),
        None => DEFERRED_TOOLS_CATALOG.to_string(),
    };
    let guide = RUN_CODE_GUIDE
        .replace("{{ deferred_tools }}", &deferred_tools)
        .replace(
            "{{ yield_time_ms }}",
            &options.default_exec_yield_time_ms.to_string(),
        );

    let mut args_map = String::new();
    let mut output_map = String::new();
    let mut current_namespace: Option<&str> = None;
    for (name, tool) in &tools {
        let namespace = tool
            .tool_name
            .namespace
            .as_deref()
            .and_then(|namespace| namespace_descriptions.get(namespace));
        let namespace_name = namespace.map(|namespace| namespace.name.as_str());
        if namespace_name != current_namespace
            && let Some(namespace) = namespace
            && !namespace.description.trim().is_empty()
        {
            for line in namespace.description.trim().lines() {
                let _ = writeln!(args_map, "  // {}", line.trim_end());
            }
        }
        current_namespace = namespace_name;
        let description = if is_search_tool(tool) {
            RUN_CODE_TOOL_SEARCH_DESCRIPTION
        } else {
            tool.description.as_str()
        };
        push_tool_entry(&mut args_map, &mut output_map, name, description, tool);
    }

    let output_types_use_mcp = output_map.contains("CallToolResult");
    let mut declarations = String::new();
    if output_types_use_mcp {
        declarations.push_str(MCP_TYPESCRIPT_PREAMBLE);
        declarations.push('\n');
    }
    declarations.push_str(RUNTIME_DECLARATIONS_HEAD);
    declarations.push('\n');
    declarations.push_str(match options.image_detail_visibility {
        ImageDetailVisibility::Visible => IMAGE_DECLARATION_WITH_DETAIL,
        ImageDetailVisibility::Hidden => IMAGE_DECLARATION,
    });
    declarations.push('\n');
    declarations.push_str(RUNTIME_DECLARATIONS_TAIL);
    let _ = write!(
        declarations,
        "\ninterface ToolArgsMap {{\n{args_map}}}\ninterface ToolOutputMap {{\n{output_map}}}\n\
type ToolName = keyof ToolArgsMap;\n\
declare const tools: {{ [K in ToolName]: (args: ToolArgsMap[K]) => Promise<ToolOutputMap[K]> }};"
    );

    format!("{RUN_CODE_INTRO}\n\n{guide}\n\nProgram-only SDK bindings:\n```ts\n{declarations}\n```")
}

fn is_search_tool(tool: &ToolDefinition) -> bool {
    tool.tool_name.is_default_namespace() && tool.tool_name.name == TOOL_SEARCH_NAME
}

/// One tool's declaration in the form of the `run_code` SDK block (its
/// entries of `ToolArgsMap` and `ToolOutputMap`), as tool search returns it.
pub fn render_run_code_tool_declaration(definition: &ToolDefinition) -> String {
    let name = normalize_code_mode_identifier(&definition.name);
    let mut args_map = String::new();
    let mut output_map = String::new();
    push_tool_entry(
        &mut args_map,
        &mut output_map,
        &name,
        &definition.description,
        definition,
    );
    format!("interface ToolArgsMap {{\n{args_map}}}\ninterface ToolOutputMap {{\n{output_map}}}")
}

/// The `wait` description when exec is offered as `run_code`.
pub fn build_run_code_wait_tool_description(compact: bool) -> &'static str {
    if compact {
        RUN_CODE_COMPACT_WAIT_DESCRIPTION
    } else {
        RUN_CODE_WAIT_DESCRIPTION
    }
}

fn push_tool_entry(
    args_map: &mut String,
    output_map: &mut String,
    name: &str,
    description: &str,
    tool: &ToolDefinition,
) {
    let key = serde_json::to_string(name).unwrap_or_else(|_| format!("\"{name}\""));
    push_doc_comment(args_map, description);
    let _ = writeln!(args_map, "  {key}: {};", definition_input_type(tool));
    let _ = writeln!(output_map, "  {key}: {};", definition_output_type(tool));
}

/// `description` as a JSDoc comment at the map entries' indent.
fn push_doc_comment(out: &mut String, description: &str) {
    let description = description.trim().replace("*/", "*\\/");
    if description.is_empty() {
        return;
    }
    let lines = description.lines().map(str::trim_end).collect::<Vec<_>>();
    if let [line] = lines.as_slice() {
        let _ = writeln!(out, "  /** {line} */");
        return;
    }
    out.push_str("  /**\n");
    for line in lines {
        if line.is_empty() {
            out.push_str("   *\n");
        } else {
            let _ = writeln!(out, "   * {line}");
        }
    }
    out.push_str("   */\n");
}

#[cfg(test)]
#[path = "run_code_tests.rs"]
mod tests;
