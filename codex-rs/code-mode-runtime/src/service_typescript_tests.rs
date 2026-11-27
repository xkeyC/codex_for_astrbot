//! Fork addition: TypeScript cells through the session API.

use std::sync::Arc;
use std::sync::Mutex;

use codex_code_mode_protocol::CodeLanguage;
use codex_code_mode_protocol::NotificationFuture;
use codex_code_mode_protocol::ToolInvocationFuture;
use codex_protocol::ToolName;
use pretty_assertions::assert_eq;
use serde_json::Value as JsonValue;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::CellId;
use super::CodeModeNestedToolCall;
use super::CodeModeSessionDelegate;
use super::InProcessCodeModeSession;
use super::RuntimeResponse;
use crate::CodeModeToolKind;
use crate::ExecuteRequest;
use crate::FunctionCallOutputContentItem;
use crate::ToolDefinition;

/// Answers every tool call with `{ "echo": <input> }` and records it.
#[derive(Default)]
struct RecordingDelegate {
    calls: Mutex<Vec<CodeModeNestedToolCall>>,
}

impl RecordingDelegate {
    fn inputs(&self) -> Vec<Option<JsonValue>> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.input.clone())
            .collect()
    }
}

impl CodeModeSessionDelegate for RecordingDelegate {
    fn invoke_tool<'a>(
        &'a self,
        invocation: CodeModeNestedToolCall,
        _cancellation_token: CancellationToken,
    ) -> ToolInvocationFuture<'a> {
        let result = json!({ "echo": &invocation.input });
        self.calls.lock().unwrap().push(invocation);
        Box::pin(async move { Ok(result) })
    }

    fn notify<'a>(
        &'a self,
        _call_id: String,
        _cell_id: CellId,
        _text: String,
        _cancellation_token: CancellationToken,
    ) -> NotificationFuture<'a> {
        Box::pin(async { Ok(()) })
    }

    fn cell_closed(&self, _cell_id: &CellId) {}
}

fn count_tool() -> ToolDefinition {
    ToolDefinition {
        name: "count".to_string(),
        tool_name: ToolName::plain("count"),
        description: "Counts.".to_string(),
        kind: CodeModeToolKind::Function,
        input_schema: Some(json!({
            "type": "object",
            "properties": { "n": { "type": "number" } },
            "required": ["n"],
            "additionalProperties": false
        })),
        input_schema_max_bytes: None,
        output_schema: None,
    }
}

fn request(language: CodeLanguage, source: &str) -> ExecuteRequest {
    ExecuteRequest {
        tool_call_id: "call-1".to_string(),
        enabled_tools: vec![count_tool()],
        source: source.to_string(),
        yield_time_ms: Some(30_000),
        max_output_tokens: None,
        language,
    }
}

async fn run(
    session: &InProcessCodeModeSession,
    request: ExecuteRequest,
    revision: Option<u64>,
    delegate: &Arc<RecordingDelegate>,
) -> (Vec<String>, Option<String>) {
    let started = session
        .execute_with_catalog(request, revision, Arc::clone(delegate) as _, None)
        .await
        .expect("the cell starts");
    match started.initial_response().await.expect("a response") {
        RuntimeResponse::Result {
            content_items,
            error_text,
            ..
        } => (
            content_items
                .into_iter()
                .map(|item| match item {
                    FunctionCallOutputContentItem::InputText { text } => text,
                    other => format!("{other:?}"),
                })
                .collect(),
            error_text,
        ),
        other => panic!("expected a result, got {other:?}"),
    }
}

#[tokio::test]
async fn a_type_error_fails_the_cell_and_calls_no_tool() {
    let session = InProcessCodeModeSession::new();
    let delegate = Arc::new(RecordingDelegate::default());
    let (output, error_text) = run(
        &session,
        request(
            CodeLanguage::TypeScript,
            "text(\"before\");\nawait tools.count({ n: \"1\" });",
        ),
        None,
        &delegate,
    )
    .await;
    assert_eq!(output, Vec::<String>::new());
    assert_eq!(
        error_text.as_deref(),
        Some(
            "TypeScript check failed: 1 error. The program did not run; fix it and call run_code again.\n\
code.ts:2:21 - error TS2322: Type 'string' is not assignable to type 'number'.\n  \
2 | await tools.count({ n: \"1\" });\n    \
|                     ^"
        )
    );
    assert_eq!(delegate.inputs(), Vec::<Option<JsonValue>>::new());
}

#[tokio::test]
async fn a_checked_program_runs_and_calls_its_tools() {
    let session = InProcessCodeModeSession::new();
    let delegate = Arc::new(RecordingDelegate::default());
    let (output, error_text) = run(
        &session,
        request(
            CodeLanguage::TypeScript,
            "interface Counted { echo: { n: number } }\n\
             const twice = <T,>(value: T): [T, T] => [value, value];\n\
             const result = (await tools.count({ n: 2 })) as Counted;\n\
             text(twice(result.echo.n).join(\",\"));\n\
             text([...new Set([1]).union(new Set([2]))].concat([3, 4].values().take(1).toArray()).join(\",\"));",
        ),
        None,
        &delegate,
    )
    .await;
    assert_eq!(
        (output, error_text),
        (vec!["2,2".to_string(), "1,2,3".to_string()], None)
    );
    assert_eq!(delegate.inputs(), vec![Some(json!({ "n": 2 }))]);
}

#[tokio::test]
async fn runtime_errors_point_at_the_typescript_source() {
    let session = InProcessCodeModeSession::new();
    let delegate = Arc::new(RecordingDelegate::default());
    let (_, error_text) = run(
        &session,
        request(
            CodeLanguage::TypeScript,
            "type Item = { id: number };\nconst items: Item[] = [];\n  throw new Error(\"boom\");",
        ),
        None,
        &delegate,
    )
    .await;
    let error_text = error_text.expect("the program fails");
    assert!(
        error_text.starts_with("Error: boom\n    at code.ts:3:9"),
        "{error_text}"
    );
}

#[tokio::test]
async fn arguments_are_checked_against_the_schema_at_run_time() {
    let session = InProcessCodeModeSession::new();
    let delegate = Arc::new(RecordingDelegate::default());
    let source = "const args = JSON.parse('{\"n\": \"5\"}');\n\
                  try { await tools.count(args); } catch (error) { text(error); }\n\
                  await tools.count({ n: 5 });";
    let (output, error_text) = run(
        &session,
        request(CodeLanguage::TypeScript, source),
        None,
        &delegate,
    )
    .await;
    assert_eq!(
        (output, error_text),
        (
            vec!["tools.count: args.n must be number, got string \"5\"".to_string()],
            None
        )
    );
    assert_eq!(delegate.inputs(), vec![Some(json!({ "n": 5 }))]);

    // JavaScript cells are neither type-checked nor schema-checked.
    let (output, error_text) = run(
        &session,
        request(CodeLanguage::JavaScript, source),
        None,
        &delegate,
    )
    .await;
    assert_eq!((output, error_text), (Vec::<String>::new(), None));
    assert_eq!(
        delegate.inputs(),
        vec![
            Some(json!({ "n": 5 })),
            Some(json!({ "n": "5" })),
            Some(json!({ "n": 5 }))
        ]
    );
}

#[tokio::test]
async fn a_catalog_revision_keeps_its_declarations() {
    let session = InProcessCodeModeSession::new();
    let delegate = Arc::new(RecordingDelegate::default());
    let (output, _) = run(
        &session,
        request(
            CodeLanguage::TypeScript,
            "await tools.count({ n: 1 }); text(\"ok\");",
        ),
        Some(1),
        &delegate,
    )
    .await;
    assert_eq!(output, vec!["ok".to_string()]);
    // A request that only references the revision is checked against its tools.
    let (_, error_text) = run(
        &session,
        ExecuteRequest {
            enabled_tools: Vec::new(),
            ..request(CodeLanguage::TypeScript, "await tools.count({ m: 1 });")
        },
        Some(1),
        &delegate,
    )
    .await;
    assert!(
        error_text
            .as_deref()
            .is_some_and(|text| text.contains("'m' does not exist in type '{ n: number; }'")),
        "{error_text:?}"
    );
    assert_eq!(delegate.inputs(), vec![Some(json!({ "n": 1 }))]);
}
