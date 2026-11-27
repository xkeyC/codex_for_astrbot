//! Fork addition: TypeScript cells (`run_code`). A cell's program is
//! type-checked by the official TypeScript compiler, running in its own V8
//! isolate on the checker thread ([`engine`]), against the declarations of
//! the cell's tools. A program with type errors does not run: the cell fails
//! with the diagnostics. A program that passes runs with its types blanked
//! out by ts-blank-space, so its lines and columns stay the model's. Its
//! nested tool calls are also checked against their input schemas
//! ([`check_arguments`]) before they leave the runtime.

pub(crate) mod engine;
mod schema;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

use codex_code_mode_protocol::CodeModeToolKind;
use codex_code_mode_protocol::RunCodeCheckerDeclarations;
use codex_code_mode_protocol::ToolDefinition;
use codex_code_mode_protocol::build_run_code_checker_declarations;
use serde_json::Value as JsonValue;

use self::engine::CheckResult;
use self::engine::Diagnostic;
pub(crate) use self::schema::check_arguments;

/// The resource name of a TypeScript cell's program: in diagnostics and in
/// the positions of its runtime errors.
pub(crate) const PROGRAM_NAME: &str = "code.ts";
const MAX_DIAGNOSTICS: usize = 20;
/// Declaration repairs before the checker gives up on the declarations.
const MAX_DECLARATION_REPAIRS: usize = 3;

/// The tool definitions of a cell catalog, in the catalog's order (a tool's
/// callback index is its index here), and the checker declarations rendered
/// from them on first use. Cached with the catalog revision, so cells of a
/// revision share one rendering.
pub(crate) struct TypeScriptCatalog {
    definitions: Vec<Arc<ToolDefinition>>,
    declarations: Mutex<Option<Arc<Declarations>>>,
}

struct Declarations {
    rendered: RunCodeCheckerDeclarations,
    text: Arc<str>,
    untyped_tools: BTreeSet<String>,
}

impl TypeScriptCatalog {
    pub(crate) fn new(definitions: Vec<Arc<ToolDefinition>>) -> Self {
        Self {
            definitions,
            declarations: Mutex::new(None),
        }
    }

    /// The input schema the arguments of the tool at `index` are checked
    /// against: function tools only.
    pub(crate) fn input_schema(&self, index: usize) -> Option<&JsonValue> {
        self.definitions
            .get(index)
            .filter(|definition| definition.kind == CodeModeToolKind::Function)
            .and_then(|definition| definition.input_schema.as_ref())
    }

    fn declarations(&self) -> Arc<Declarations> {
        let mut declarations = self
            .declarations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Arc::clone(declarations.get_or_insert_with(|| self.render(BTreeSet::new())))
    }

    fn render(&self, untyped_tools: BTreeSet<String>) -> Arc<Declarations> {
        let definitions = self
            .definitions
            .iter()
            .map(|definition| definition.as_ref().clone())
            .collect::<Vec<_>>();
        let rendered = build_run_code_checker_declarations(&definitions, &untyped_tools);
        Arc::new(Declarations {
            text: rendered.text.as_str().into(),
            rendered,
            untyped_tools,
        })
    }

    /// Declares the tools at the declarations' `lines` untyped (`any`), for
    /// declarations that do not parse there; `None` when no tool is there.
    fn repair(&self, broken: &Declarations, lines: &[usize]) -> Option<Arc<Declarations>> {
        let tools = broken.rendered.tools_at_lines(lines);
        if tools.is_subset(&broken.untyped_tools) {
            return None;
        }
        tracing::warn!(
            tools = ?tools,
            "code-mode TypeScript declarations do not parse; these tools are checked as `any`"
        );
        let mut untyped_tools = broken.untyped_tools.clone();
        untyped_tools.extend(tools);
        let repaired = self.render(untyped_tools);
        *self
            .declarations
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&repaired));
        Some(repaired)
    }
}

/// The JavaScript to run for a TypeScript cell's `source`, or the cell's
/// failure text: the type errors, or why the check could not run (the
/// program never runs unchecked).
pub(crate) fn prepare_program(catalog: &TypeScriptCatalog, source: &str) -> Result<String, String> {
    let mut declarations = catalog.declarations();
    for _ in 0..=MAX_DECLARATION_REPAIRS {
        let result = engine::check(Arc::clone(&declarations.text), source.to_string())
            .map_err(|error| checker_failure(&error))?;
        match result {
            CheckResult::Passed { javascript } => return Ok(javascript),
            CheckResult::Failed { diagnostics } => {
                return Err(format_diagnostics(source, &diagnostics));
            }
            CheckResult::DeclarationErrors { lines } => {
                declarations = catalog.repair(&declarations, &lines).ok_or_else(|| {
                    checker_failure("the tool declarations do not parse as TypeScript")
                })?;
            }
        }
    }
    Err(checker_failure(
        "the tool declarations do not parse as TypeScript",
    ))
}

fn checker_failure(reason: &str) -> String {
    format!(
        "TypeScript check could not run: {reason}. The program did not run; call run_code again."
    )
}

/// tsc-style diagnostics with a code frame, positions in the model's code.
pub(crate) fn format_diagnostics(source: &str, diagnostics: &[Diagnostic]) -> String {
    let count = diagnostics.len();
    let mut text = format!(
        "TypeScript check failed: {count} error{}. The program did not run; fix {} and call run_code again.",
        if count == 1 { "" } else { "s" },
        if count == 1 { "it" } else { "them" },
    );
    let lines = source.lines().collect::<Vec<_>>();
    for diagnostic in diagnostics.iter().take(MAX_DIAGNOSTICS) {
        let code = if diagnostic.code == 0 {
            String::new()
        } else {
            format!(" TS{}", diagnostic.code)
        };
        let message = diagnostic.message.replace('\n', "\n  ");
        if diagnostic.file.is_none() || diagnostic.line == 0 {
            let _ = write!(text, "\nerror{code}: {message}");
            continue;
        }
        let _ = write!(
            text,
            "\n{PROGRAM_NAME}:{}:{} - error{code}: {message}",
            diagnostic.line, diagnostic.column
        );
        if let Some(line) = lines.get(diagnostic.line - 1) {
            let number = diagnostic.line.to_string();
            let (caret_indent, caret_width) =
                caret(line, diagnostic.column.saturating_sub(1), diagnostic.length);
            let _ = write!(
                text,
                "\n  {number} | {line}\n  {} | {caret_indent}{}",
                " ".repeat(number.len()),
                "^".repeat(caret_width)
            );
        }
    }
    if count > MAX_DIAGNOSTICS {
        let _ = write!(text, "\n+{} more", count - MAX_DIAGNOSTICS);
    }
    text
}

const MAX_CARET_WIDTH: usize = 40;

/// The indent (tabs kept, for alignment) and width of a caret under the
/// diagnostic's span: `column` and `length` count UTF-16 units.
fn caret(line: &str, column: usize, length: usize) -> (String, usize) {
    let mut indent = String::new();
    let mut units = 0;
    let mut chars = line.chars().peekable();
    while let Some(&ch) = chars.peek() {
        if units >= column {
            break;
        }
        indent.push(if ch == '\t' { '\t' } else { ' ' });
        units += ch.len_utf16();
        chars.next();
    }
    let mut width = 0;
    let mut covered = 0;
    for ch in chars {
        if covered >= length {
            break;
        }
        covered += ch.len_utf16();
        width += 1;
    }
    (indent, width.clamp(1, MAX_CARET_WIDTH))
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
