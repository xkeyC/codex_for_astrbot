use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use codex_code_mode_protocol::CodeModeToolKind;
use codex_code_mode_protocol::ImageDetailVisibility;
use codex_code_mode_protocol::RunCodeDescriptionOptions;
use codex_code_mode_protocol::ToolDefinition;
use codex_code_mode_protocol::ToolNamespaceDescription;
use codex_code_mode_protocol::build_run_code_tool_description;
use codex_protocol::ToolName;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::TypeScriptCatalog;
use super::engine;
use super::engine::CheckResult;
use super::engine::JobKind;
use super::prepare_program;

fn web_search() -> ToolDefinition {
    ToolDefinition {
        name: "web_search".to_string(),
        tool_name: ToolName::plain("web_search"),
        description: "Searches the web.".to_string(),
        kind: CodeModeToolKind::Function,
        input_schema: Some(json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "limit": { "type": "number" },
                "recency": { "type": "string", "enum": ["day", "week"] }
            },
            "required": ["query"],
            "additionalProperties": false
        })),
        input_schema_max_bytes: None,
        output_schema: Some(json!({
            "type": "object",
            "properties": {
                "results": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": { "title": { "type": "string" }, "url": { "type": "string" } },
                        "required": ["title", "url"]
                    }
                }
            },
            "required": ["results"]
        })),
    }
}

/// A host tool without an output schema, like AstrBot's.
fn history() -> ToolDefinition {
    ToolDefinition {
        name: "astrbot__get_history".to_string(),
        tool_name: ToolName::namespaced("astrbot__", "get_history"),
        description: "Reads a group's history.\nNewest first.".to_string(),
        kind: CodeModeToolKind::Function,
        input_schema: Some(json!({
            "type": "object",
            "properties": {
                "group_id": { "type": "string", "description": "The group." },
                "count": { "type": "integer" }
            },
            "required": ["group_id"]
        })),
        input_schema_max_bytes: None,
        output_schema: None,
    }
}

/// A deferred tool: in the cell's catalog, not in the prompt.
fn vault() -> ToolDefinition {
    ToolDefinition {
        name: "astrbot__vault".to_string(),
        tool_name: ToolName::namespaced("astrbot__", "vault"),
        description: "Opens the vault.".to_string(),
        kind: CodeModeToolKind::Function,
        input_schema: Some(
            json!({ "type": "object", "properties": { "code": { "type": "number" } } }),
        ),
        input_schema_max_bytes: None,
        output_schema: None,
    }
}

fn tool_search() -> ToolDefinition {
    ToolDefinition {
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

fn catalog() -> TypeScriptCatalog {
    TypeScriptCatalog::new(
        [
            web_search(),
            history(),
            vault(),
            tool_search(),
            apply_patch(),
        ]
        .into_iter()
        .map(Arc::new)
        .collect(),
    )
}

fn check(source: &str) -> Result<String, String> {
    prepare_program(&catalog(), source)
}

fn errors(source: &str) -> String {
    check(source).expect_err("the program has type errors")
}

#[test]
fn reports_errors_like_tsc_with_a_code_frame() {
    assert_eq!(
        errors(
            "// look it up\nconst r = await tools.web_search({ query: \"x\", limit: \"5\" });\ninspect(r.results.map((x) => x.titel));"
        ),
        r#"TypeScript check failed: 2 errors. The program did not run; fix them and call run_code again.
code.ts:2:48 - error TS2322: Type 'string' is not assignable to type 'number'.
  2 | const r = await tools.web_search({ query: "x", limit: "5" });
    |                                                ^^^^^
code.ts:3:32 - error TS2551: Property 'titel' does not exist on type '{ title: string; url: string; }'. Did you mean 'title'?
  3 | inspect(r.results.map((x) => x.titel));
    |                                ^^^^^"#
    );
}

#[test]
fn catches_wrong_missing_unknown_and_misspelled_fields() {
    for (source, expected) in [
        (
            "await tools.web_search({ limit: 5 });",
            "Property 'query' is missing in type '{ limit: number; }'",
        ),
        (
            "await tools.web_search({ query: \"x\", limt: 5 });",
            "error TS2561: Object literal may only specify known properties, but 'limt' does not exist",
        ),
        (
            "await tools.web_search({ query: \"x\", recency: \"month\" });",
            "error TS2322: Type '\"month\"' is not assignable to type '\"day\" | \"week\"'.",
        ),
        (
            "await tools.astrbot__get_history({ group_id: 123 });",
            "error TS2322: Type 'number' is not assignable to type 'string'.",
        ),
        (
            "await tools.no_such_tool({});",
            "error TS2339: Property 'no_such_tool' does not exist",
        ),
        ("await tools.apply_patch({ patch: 1 });", "error TS2345"),
        ("console.log(1);", "Cannot find name 'console'."),
    ] {
        let text = errors(source);
        assert!(text.contains(expected), "{source}\n{text}");
    }
}

#[test]
fn erasable_typescript_runs_with_its_types_blanked() {
    let source = r#"interface Hit { title: string; url: string }
type Pair<T> = [T, T];
const first = <T,>(items: T[]): T | undefined => items[0];
const found = await tools.web_search({ query: "x", recency: "day" });
const hit = first<Hit>(found.results);
const titles = found.results.map((h: Hit) => h.title) satisfies string[];
const pair: Pair<number> = [1, 2];
const seen: Record<string, number> = {};
for (const h of found.results) seen[h.url] = (seen[h.url] ?? 0) + 1;
// Results without a declared shape, and stored values, are `any`.
const history = await tools.astrbot__get_history({ group_id: "g", count: 3 });
inspect(history.content[0].text, load("notes").items.length);
const settled = await Promise.allSettled([tools.astrbot__vault({ code: 7 }), tools.apply_patch("*** Begin Patch")]);
const name: string = ALL_TOOLS.find((tool) => tool.name.includes("vault"))!.name;
const searched = await tools.tool_search({ query: "vault", limit: 3 });
inspect(await tools[name]({ code: 1 }), searched.tools, settled.length, hit?.title as string, titles, pair);
text(seen); notify("progress"); store("notes", { items: [] });
image("data:image/png;base64,AAAA", "low");
generatedImage({ image_url: "data:image/png;base64,AAAA", output_hint: "saved" });
content([{ type: "text", text: "x" }]); audio("data:audio/wav;base64,AAAA");
clearTimeout(setTimeout(() => yield_control(), 10));
await new Promise((resolve) => setTimeout(resolve, 1));
class Counter { private count = 0; add(n: number): this { this.count += n; return this; } }
inspect(new Counter().add(1));
exit();
"#;
    let javascript = check(source).expect("the program type-checks");
    assert_eq!(javascript.lines().count(), source.lines().count());
    for (typescript, javascript) in source.lines().zip(javascript.lines()) {
        assert_eq!(typescript.len(), javascript.len(), "{javascript}");
    }
    assert!(javascript.contains("const pair ") && !javascript.contains("Pair<number>"));
}

#[test]
fn non_erasable_syntax_is_rejected() {
    for source in [
        "enum Color { Red }\ninspect(Color.Red);",
        "namespace Util { export const x = 1; }\ninspect(Util.x);",
        "class Point { constructor(public x: number) {} }\ninspect(new Point(1));",
    ] {
        let text = errors(source);
        assert!(
            text.contains(
                "error TS1294: This syntax is not allowed when 'erasableSyntaxOnly' is enabled."
            ),
            "{source}\n{text}"
        );
    }
}

#[test]
fn syntax_errors_come_alone_and_the_list_is_capped() {
    let text = errors("const s = (x: number => 1;\nconst n: number = \"a\";");
    assert!(text.starts_with("TypeScript check failed: 1 error. The program did not run; fix it"));
    assert!(
        text.contains("code.ts:1:22 - error TS1005: ',' expected."),
        "{text}"
    );

    let source = (0..25)
        .map(|index| format!("const v{index}: number = \"{index}\";"))
        .collect::<Vec<_>>()
        .join("\n");
    let text = errors(&source);
    assert!(
        text.starts_with("TypeScript check failed: 25 errors."),
        "{text}"
    );
    assert!(text.ends_with("\n+5 more"), "{text}");
    assert_eq!(text.matches(" - error TS2322").count(), 20);
}

#[test]
fn the_prompt_declarations_compile() {
    let namespaces = BTreeMap::from([(
        "astrbot__".to_string(),
        ToolNamespaceDescription {
            name: "astrbot__".to_string(),
            description: "Host tools.\nUse them freely.".to_string(),
        },
    )]);
    let mcp = ToolDefinition {
        name: "mcp__docs__read".to_string(),
        tool_name: ToolName::namespaced("mcp__docs__", "read"),
        output_schema: Some(json!({
            "type": "object",
            "properties": {
                "content": { "type": "array", "items": { "type": "object" } },
                "isError": { "type": "boolean" },
                "_meta": { "type": "object" },
                "structuredContent": { "type": "object", "properties": { "body": { "type": "string" } } }
            }
        })),
        ..vault()
    };
    for visibility in [
        ImageDetailVisibility::Hidden,
        ImageDetailVisibility::Visible,
    ] {
        let description = build_run_code_tool_description(
            &[
                web_search(),
                history(),
                tool_search(),
                apply_patch(),
                mcp.clone(),
            ],
            &namespaces,
            RunCodeDescriptionOptions {
                default_exec_yield_time_ms: 10_000,
                image_detail_visibility: visibility,
            },
        );
        let block = description
            .split_once("```ts\n")
            .and_then(|(_, block)| block.strip_suffix("\n```"))
            .expect("the description ends with the SDK block");
        assert!(block.contains("type CallToolResult"));
        let result = engine::check(
            block.into(),
            "const r = await tools.mcp__docs__read({ code: 1 });\ninspect(r.structuredContent?.body, ALL_TOOLS);".to_string(),
        );
        assert!(
            matches!(result, Ok(CheckResult::Passed { .. })),
            "{result:?}"
        );
    }
}

#[test]
fn declarations_that_do_not_parse_are_reported_by_line() {
    let result = engine::check(
        "declare function inspect(...values: unknown[]): void;\ninterface ToolArgsMap {\n  \"x\": { a: ; };\n}".into(),
        "inspect(1);".to_string(),
    );
    assert_eq!(
        result,
        Ok(CheckResult::DeclarationErrors { lines: vec![2] })
    );
}

#[test]
fn a_stopped_checker_recovers() {
    let timed_out = engine::run_job(JobKind::Script("for (;;) {}"), Duration::from_millis(200));
    assert_eq!(
        timed_out,
        Err("the TypeScript check took longer than 0.2 s".to_string())
    );
    assert!(check("inspect(1);").is_ok());

    let out_of_memory = engine::run_job(
        JobKind::Script("const chunks = []; for (;;) chunks.push(new Array(1e6).fill(1.5));"),
        engine::CHECK_TIMEOUT,
    );
    assert_eq!(
        out_of_memory,
        Err("the TypeScript check ran out of memory (256 MB)".to_string())
    );
    assert!(check("inspect(1);").is_ok());
}

#[test]
fn a_declaration_over_its_budget_is_unknown_like_in_the_prompt() {
    let source = "await tools.web_search({ anything: 1 });";
    let over_budget = TypeScriptCatalog::new(vec![Arc::new(ToolDefinition {
        input_schema_max_bytes: Some(10),
        ..web_search()
    })]);
    assert!(prepare_program(&over_budget, source).is_ok());
    let within_budget = TypeScriptCatalog::new(vec![Arc::new(web_search())]);
    assert!(prepare_program(&within_budget, source).is_err());
}
