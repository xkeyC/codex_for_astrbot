use super::*;
use crate::common::Reasoning;
use crate::common::ResponsesApiTools;
use crate::common::TextControls;
use crate::common::TextFormat;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use pretty_assertions::assert_eq;
use serde_json::value::RawValue;

fn request(input: Vec<ResponseItem>, tools: Value) -> ResponsesApiRequest {
    let tools: Arc<RawValue> = serde_json::from_str::<Box<RawValue>>(&tools.to_string())
        .unwrap()
        .into();
    ResponsesApiRequest {
        model: "deepseek-v4.1-flash".to_string(),
        instructions: "You are a voice.".to_string(),
        input,
        tools: Some(ResponsesApiTools::from(tools)),
        tool_choice: "auto".to_string(),
        parallel_tool_calls: true,
        reasoning: None,
        store: false,
        stream: true,
        stream_options: None,
        include: Vec::new(),
        service_tier: None,
        prompt_cache_key: Some("thread-1".to_string()),
        text: None,
        client_metadata: None,
        access_programs: None,
    }
}

fn message(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![if role == "assistant" {
            ContentItem::OutputText {
                text: text.to_string(),
            }
        } else {
            ContentItem::InputText {
                text: text.to_string(),
            }
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn reasoning(text: &str) -> ResponseItem {
    reasoning_item(text.to_string())
}

fn call(call_id: &str, namespace: Option<&str>, name: &str, arguments: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: namespace.map(str::to_string),
        arguments: arguments.to_string(),
        encrypted_function_args: None,
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn output(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: Some(call_id.to_string()),
        name: None,
        namespace: None,
        output: FunctionCallOutputPayload::from_text(text.to_string()),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn tools() -> Value {
    json!([
        {"type": "function", "name": "backend_task", "description": "Hand a task off.",
         "strict": false, "parameters": {"type": "object", "properties": {"task": {"type": "string"}}}},
        {"type": "namespace", "name": "mcp__homeassistant__", "description": "HA",
         "tools": [{"type": "function", "name": "get_state", "description": "State.",
                    "parameters": {"type": "object", "properties": {}}}]},
        {"type": "custom", "name": "apply_patch", "description": "Edit files.",
         "format": {"type": "grammar", "syntax": "lark", "definition": "start: /.+/"}},
        {"type": "web_search"}
    ])
}

#[test]
fn a_turn_becomes_chat_messages() {
    let input = vec![
        message("developer", "The call started."),
        message("user", "What time is it?"),
        reasoning("They want the time."),
        message("assistant", "Let me check."),
        call("call_1", None, "backend_task", r#"{"task":"time"}"#),
        call("call_2", Some("mcp__homeassistant__"), "get_state", "{}"),
        output("call_1", "15:40"),
        output("call_2", "on"),
        reasoning("Now answer."),
        message("assistant", "It is 15:40."),
    ];
    let (body, _) = chat_request(&request(input, tools()), &ChatOptions::default());
    assert_eq!(
        body["messages"],
        json!([
            {"role": "system", "content": "You are a voice."},
            {"role": "user", "content": "<system>
The call started.
</system>"},
            {"role": "user", "content": "What time is it?"},
            {"role": "assistant", "content": "Let me check.",
             "reasoning_content": "They want the time.",
             "tool_calls": [
                {"id": "call_1", "type": "function",
                 "function": {"name": "backend_task", "arguments": r#"{"task":"time"}"#}},
                {"id": "call_2", "type": "function",
                 "function": {"name": "mcp__homeassistant__get_state", "arguments": "{}"}}
             ]},
            {"role": "tool", "tool_call_id": "call_1", "content": "15:40"},
            {"role": "tool", "tool_call_id": "call_2", "content": "on"},
            {"role": "assistant", "content": "It is 15:40.", "reasoning_content": "Now answer."}
        ])
    );
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
    assert_eq!(body["tool_choice"], json!("auto"));
    assert_eq!(body["parallel_tool_calls"], json!(true));
}

#[test]
fn tools_are_chat_functions() {
    let (body, names) = chat_request(&request(Vec::new(), tools()), &ChatOptions::default());
    let tools = body["tools"].as_array().unwrap();
    let names_sent: Vec<&str> = tools
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    // Web search has no chat form.
    assert_eq!(
        names_sent,
        [
            "backend_task",
            "mcp__homeassistant__get_state",
            "apply_patch"
        ]
    );
    assert_eq!(
        tools[2]["function"]["parameters"]["required"],
        json!(["input"])
    );
    assert!(
        tools[2]["function"]["parameters"]["properties"]["input"]["description"]
            .as_str()
            .unwrap()
            .contains("start: /.+/")
    );
    assert_eq!(
        names.target("mcp__homeassistant__get_state"),
        ToolTarget {
            namespace: Some("mcp__homeassistant__".to_string()),
            name: "get_state".to_string(),
            freeform: false,
        }
    );
    assert!(names.target("apply_patch").freeform);
}

#[test]
fn tool_names_fit_chat_apis() {
    let mut names = ToolNames::default();
    assert_eq!(
        names.add(Some("my.server"), "get state", false),
        "my_server__get_state"
    );
    let long = "x".repeat(80);
    let first = names.add(None, &long, false);
    let second = names.add(Some("x"), &"x".repeat(80), false);
    assert_eq!(first.len(), 64);
    assert_eq!(second.len(), 64);
    assert_ne!(first, second);
    assert_eq!(names.target(&second).namespace.as_deref(), Some("x"));
}

#[test]
fn calls_and_outputs_out_of_order_still_make_a_valid_history() {
    let input = vec![
        message("user", "Hi"),
        // A call nobody answered (an aborted turn), then an output whose call
        // is gone (compacted away).
        call("call_1", None, "backend_task", "{}"),
        message("user", "Hello?"),
        output("call_9", "late result"),
        ResponseItem::CustomToolCall {
            id: None,
            status: None,
            call_id: "call_3".to_string(),
            name: "apply_patch".to_string(),
            namespace: None,
            input: "*** Begin Patch".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    let (body, _) = chat_request(&request(input, tools()), &ChatOptions::default());
    assert_eq!(
        body["messages"].as_array().unwrap()[2..],
        [
            json!({"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_1", "type": "function",
                 "function": {"name": "backend_task", "arguments": "{}"}}]}),
            json!({"role": "tool", "tool_call_id": "call_1", "content": "aborted"}),
            json!({"role": "user", "content": "Hello?"}),
            json!({"role": "user", "content": "(Tool output)\nlate result"}),
            json!({"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_3", "type": "function",
                 "function": {"name": "apply_patch", "arguments": r#"{"input":"*** Begin Patch"}"#}}]}),
            json!({"role": "tool", "tool_call_id": "call_3", "content": "aborted"}),
        ]
    );
}

#[test]
fn effort_schema_and_extra_body_shape_the_request() {
    let mut req = request(vec![message("user", "Hi")], json!([]));
    req.reasoning = Some(Reasoning {
        effort: Some(ReasoningEffortConfig::High),
        summary: None,
        context: None,
    });
    req.text = Some(TextControls {
        verbosity: None,
        format: Some(TextFormat {
            r#type: Default::default(),
            strict: true,
            schema: json!({"type": "object"}),
            name: "answer".to_string(),
        }),
    });
    let mut extra_body = Map::new();
    extra_body.insert("thinking".to_string(), json!({"type": "disabled"}));
    extra_body.insert("parallel_tool_calls".to_string(), Value::Null);
    extra_body.insert("reasoning_effort".to_string(), Value::Null);
    let (body, _) = chat_request(
        &req,
        &ChatOptions {
            extra_body,
            remove: vec!["stream_options".to_string()],
        },
    );
    assert!(body.get("stream_options").is_none());
    assert_eq!(body["thinking"], json!({"type": "disabled"}));
    assert!(body.get("reasoning_effort").is_none());
    // No tools: none of the tool fields.
    assert!(body.get("tools").is_none());
    assert_eq!(
        body["response_format"],
        json!({"type": "json_schema",
               "json_schema": {"name": "answer", "schema": {"type": "object"}, "strict": false}})
    );

    let (body, _) = chat_request(&req, &ChatOptions::default());
    assert_eq!(body["reasoning_effort"], json!("high"));
}

fn sse(chunks: &[Value], done: bool) -> String {
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    if done {
        body.push_str("data: [DONE]\n\n");
    }
    body
}

async fn events(body: String, tools: ToolNames) -> Vec<Result<ResponseEvent, ApiError>> {
    let stream = futures::stream::iter(vec![Ok::<_, codex_client::TransportError>(
        bytes::Bytes::from(body),
    )]);
    let (tx, mut rx) = mpsc::channel(64);
    process_chat_sse(stream, tx, Duration::from_secs(5), None, tools).await;
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        out.push(event);
    }
    out
}

fn describe(event: &Result<ResponseEvent, ApiError>) -> String {
    match event {
        Ok(ResponseEvent::Created { response_id }) => format!("created {response_id:?}"),
        Ok(ResponseEvent::OutputItemAdded(item)) => format!("added {}", kind(item)),
        Ok(ResponseEvent::OutputItemDone(item)) => format!("done {}", kind(item)),
        Ok(ResponseEvent::OutputTextDelta(text)) => format!("text {text}"),
        Ok(ResponseEvent::ReasoningContentDelta { delta, .. }) => format!("reasoning {delta}"),
        Ok(ResponseEvent::Completed {
            response_id,
            token_usage,
            ..
        }) => format!(
            "completed {response_id} {:?}",
            token_usage.as_ref().map(|usage| (
                usage.input_tokens,
                usage.cached_input_tokens,
                usage.output_tokens
            ))
        ),
        Ok(other) => format!("{other:?}"),
        Err(err) => format!("error {err}"),
    }
}

fn kind(item: &ResponseItem) -> String {
    match item {
        ResponseItem::Message { content, .. } => format!("message {:?}", message_text(content)),
        ResponseItem::Reasoning { content, .. } => {
            format!("reasoning {:?}", reasoning_text(&[], content.as_deref()))
        }
        ResponseItem::FunctionCall {
            namespace,
            name,
            arguments,
            call_id,
            ..
        } => format!("call {call_id} {namespace:?} {name} {arguments}"),
        ResponseItem::CustomToolCall {
            name,
            input,
            call_id,
            ..
        } => format!("custom {call_id} {name} {input}"),
        other => format!("{other:?}"),
    }
}

#[tokio::test]
async fn reasoning_and_text_stream_and_usage_completes() {
    let body = sse(
        &[
            json!({"id": "c1", "choices": [{"index": 0, "delta": {"role": "assistant", "reasoning_content": "Think"}}]}),
            json!({"id": "c1", "choices": [{"index": 0, "delta": {"reasoning_content": "ing."}}]}),
            json!({"id": "c1", "choices": [{"index": 0, "delta": {"content": "Hi"}}]}),
            json!({"id": "c1", "choices": [{"index": 0, "delta": {"content": " there."}, "finish_reason": "stop"}]}),
            json!({"id": "c1", "choices": [], "usage": {"prompt_tokens": 100, "completion_tokens": 7,
                   "total_tokens": 107, "prompt_cache_hit_tokens": 96}}),
        ],
        /*done*/ true,
    );
    let described: Vec<String> = events(body, ToolNames::default())
        .await
        .iter()
        .map(describe)
        .collect();
    assert_eq!(
        described,
        [
            "created Some(\"c1\")",
            "added reasoning \"\"",
            "reasoning Think",
            "reasoning ing.",
            "done reasoning \"Thinking.\"",
            "added message \"\"",
            "text Hi",
            "text  there.",
            "done message \"Hi there.\"",
            "completed c1 Some((100, 96, 7))",
        ]
    );
}

#[tokio::test]
async fn tool_calls_gather_across_chunks() {
    let (_, names) = chat_request(&request(Vec::new(), tools()), &ChatOptions::default());
    let body = sse(
        &[
            json!({"id": "c2", "choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_a", "type": "function",
                 "function": {"name": "mcp__homeassistant__get_state", "arguments": "{\"en"}}]}}]}),
            json!({"id": "c2", "choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "tity\":1}"}},
                {"index": 1, "id": "call_b", "function": {"name": "apply_patch", "arguments": "{\"input\":\"P\"}"}}]}}]}),
            json!({"id": "c2", "choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        ],
        /*done*/ true,
    );
    let described: Vec<String> = events(body, names).await.iter().map(describe).collect();
    assert_eq!(
        described,
        [
            "created Some(\"c2\")",
            "done call call_a Some(\"mcp__homeassistant__\") get_state {\"entity\":1}",
            "done custom call_b apply_patch P",
            "completed c2 None",
        ]
    );
}

#[tokio::test]
async fn an_error_or_an_unfinished_stream_fails() {
    let body = sse(
        &[json!({"error": {"message": "This model's maximum context length is 128000 tokens"}})],
        /*done*/ false,
    );
    let described: Vec<String> = events(body, ToolNames::default())
        .await
        .iter()
        .map(describe)
        .collect();
    assert_eq!(described, ["error context window exceeded"]);

    let body = sse(
        &[json!({"id": "c3", "choices": [{"delta": {"content": "Hal"}}]})],
        /*done*/ false,
    );
    let described: Vec<String> = events(body, ToolNames::default())
        .await
        .iter()
        .map(describe)
        .collect();
    assert_eq!(
        described.last().unwrap(),
        "error stream error: stream closed before the chat completion finished"
    );
}

#[tokio::test]
async fn a_finished_stream_without_done_completes() {
    let body = sse(
        &[json!({"id": "c4", "choices": [{"delta": {"content": "OK"}, "finish_reason": "stop"}]})],
        /*done*/ false,
    );
    let described: Vec<String> = events(body, ToolNames::default())
        .await
        .iter()
        .map(describe)
        .collect();
    assert_eq!(described.last().unwrap(), "completed c4 None");
}

#[tokio::test]
async fn empty_tool_call_lists_and_shared_indexes_are_handled() {
    let body = sse(
        &[
            json!({"id": "c5", "choices": [{"delta": {"reasoning_content": "A", "tool_calls": []}}]}),
            json!({"id": "c5", "choices": [{"delta": {"reasoning_content": "B", "tool_calls": []}}]}),
            // Two parallel calls, both at index 0.
            json!({"id": "c5", "choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_x", "function": {"name": "backend_task", "arguments": "{}"}}]}}]}),
            json!({"id": "c5", "choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_y", "function": {"name": "backend_task", "arguments": "{\"a\":1}"}}]}}]}),
            json!({"id": "c5", "choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        ],
        /*done*/ true,
    );
    let described: Vec<String> = events(body, ToolNames::default())
        .await
        .iter()
        .map(describe)
        .collect();
    assert_eq!(
        described,
        [
            "created Some(\"c5\")",
            "added reasoning \"\"",
            "reasoning A",
            "reasoning B",
            "done reasoning \"AB\"",
            "done call call_x None backend_task {}",
            "done call call_y None backend_task {\"a\":1}",
            "completed c5 None",
        ]
    );
}

#[tokio::test]
async fn a_call_cut_off_at_the_output_limit_fails() {
    let body = sse(
        &[json!({"id": "c6", "choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "call_z", "function": {"name": "backend_task", "arguments": "{\"ta"}}]},
            "finish_reason": "length"}]})],
        /*done*/ true,
    );
    let described: Vec<String> = events(body, ToolNames::default())
        .await
        .iter()
        .map(describe)
        .collect();
    assert_eq!(
        described.last().unwrap(),
        "error stream error: the response was cut off at the output token limit"
    );
}

#[test]
fn a_steps_texts_go_on_lines_of_their_own() {
    let input = vec![
        message("user", "Hi"),
        reasoning("First."),
        reasoning("Second."),
        message("assistant", "One."),
        message("assistant", "Two."),
    ];
    let (body, _) = chat_request(&request(input, json!([])), &ChatOptions::default());
    assert_eq!(
        body["messages"][2],
        json!({"role": "assistant", "content": "One.
Two.", "reasoning_content": "First.
Second."})
    );
}

#[tokio::test]
async fn pieces_follow_their_call_when_indexes_are_shared() {
    let body = sse(
        &[
            json!({"id": "c7", "choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_x", "function": {"name": "backend_task", "arguments": "{"}}]}}]}),
            json!({"id": "c7", "choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "}"}}]}}]}),
            json!({"id": "c7", "choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_y", "function": {"name": "backend_task", "arguments": "{\"a\""}}]}}]}),
            json!({"id": "c7", "choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": ":1}"}}]}}]}),
            json!({"id": "c7", "choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        ],
        /*done*/ true,
    );
    let described: Vec<String> = events(body, ToolNames::default())
        .await
        .iter()
        .map(describe)
        .collect();
    assert_eq!(
        described[1..3],
        [
            "done call call_x None backend_task {}",
            "done call call_y None backend_task {\"a\":1}",
        ]
    );
}

#[tokio::test]
async fn reasoning_cut_off_before_any_answer_fails() {
    let body = sse(
        &[
            json!({"id": "c8", "choices": [{"delta": {"reasoning_content": "Hmm"}, "finish_reason": "length"}]}),
        ],
        /*done*/ true,
    );
    let described: Vec<String> = events(body, ToolNames::default())
        .await
        .iter()
        .map(describe)
        .collect();
    assert_eq!(
        described.last().unwrap(),
        "error stream error: the response was cut off at the output token limit"
    );
}
