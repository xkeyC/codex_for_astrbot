//! Fork addition (AstrBot): the Chat Completions wire, `POST /chat/completions`,
//! for providers whose Responses endpoint is missing or broken.
//!
//! A turn's request is built as for Responses ([`ResponsesApiRequest`]) and
//! translated here; the streamed chunks come back as the same
//! [`ResponseEvent`]s.
//!
//! - The instructions are the one `system` message, at the start; developer
//!   messages are user ones, in a `<system>` tag (a system message mid-way
//!   may be moved to the front, breaking the prefix cache).
//! - An assistant step (its reasoning, text and tool calls) is one `assistant`
//!   message; its reasoning goes back as `reasoning_content`.
//! - Function tools go as they are; a namespace's tools flattened
//!   (`<namespace>__<name>`); a freeform tool as a function taking
//!   `{"input": string}`. Hosted tools (web search, tool search) have no chat
//!   form and are left out.
//! - Tool outputs are `tool` messages. One whose call is not in the history
//!   becomes a user message; a call left without output gets an `aborted` one.
//! - `reasoning.effort` is `reasoning_effort`, an output schema
//!   `response_format` (not strict); token usage comes with
//!   `stream_options.include_usage`. The provider's extra body is merged over
//!   the top level, and the fields it rejects are removed.

use crate::auth::SharedAuthProvider;
use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::common::ResponsesApiRequest;
use crate::endpoint::responses::ResponsesOptions;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::Compression;
use crate::requests::headers::build_session_headers;
use crate::requests::headers::insert_header;
use crate::requests::headers::subagent_header;
use crate::telemetry::SseTelemetry;
use codex_client::EncodedJsonBody;
use codex_client::HttpTransport;
use codex_client::RequestCompression;
use codex_client::RequestTelemetry;
use codex_client::StreamResponse;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ImageReference;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::Stream;
use futures::StreamExt;
use http::HeaderValue;
use http::Method;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;
use tracing::debug;
use tracing::instrument;
use tracing::trace;

/// Longest function name chat APIs take (`^[a-zA-Z0-9_-]{1,64}$`).
const MAX_TOOL_NAME_LEN: usize = 64;
/// What a call left without output says (a chat history must answer every
/// call before it goes on).
const ABORTED_OUTPUT: &str = "aborted";

/// How requests to a chat provider are shaped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatOptions {
    /// Merged over the request body's top level (a null value removes the
    /// field).
    pub extra_body: Map<String, Value>,
    /// Top-level fields left out of the request body (`reasoning_effort`
    /// for a provider that rejects it, say).
    pub remove: Vec<String>,
}

pub struct ChatClient<T: HttpTransport> {
    session: EndpointSession<T>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
}

impl<T: HttpTransport> ChatClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
            sse_telemetry: None,
        }
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
            sse_telemetry: sse,
        }
    }

    /// Streams `request`, sent as a chat completion.
    #[instrument(
        name = "chat.stream_request",
        level = "info",
        skip_all,
        fields(
            transport = "chat_http",
            http.method = "POST",
            api.path = "/chat/completions"
        )
    )]
    pub async fn stream_request(
        &self,
        request: ResponsesApiRequest,
        options: ResponsesOptions,
        chat: &ChatOptions,
    ) -> Result<ResponseStream, ApiError> {
        let ResponsesOptions {
            session_id,
            thread_id,
            session_source,
            extra_headers,
            compression,
            turn_state: _,
        } = options;
        let (body, tools) = chat_request(&request, chat);
        let body = EncodedJsonBody::encode(&body)
            .map_err(|e| ApiError::Stream(format!("failed to encode chat request: {e}")))?;

        let mut headers = extra_headers;
        if let Some(ref thread_id) = thread_id {
            insert_header(&mut headers, "x-client-request-id", thread_id);
        }
        headers.extend(build_session_headers(session_id, thread_id));
        if let Some(subagent) = subagent_header(&session_source) {
            insert_header(&mut headers, "x-openai-subagent", &subagent);
        }
        let request_compression = match compression {
            Compression::None => RequestCompression::None,
            Compression::Zstd => RequestCompression::Zstd,
        };

        let stream_response = self
            .session
            .stream_encoded_json_with(
                Method::POST,
                "/chat/completions",
                headers,
                Some(body),
                |req| {
                    req.headers.insert(
                        http::header::ACCEPT,
                        HeaderValue::from_static("text/event-stream"),
                    );
                    req.compression = request_compression;
                },
            )
            .await?;

        Ok(spawn_chat_stream(
            stream_response,
            self.session.provider().stream_idle_timeout,
            self.sse_telemetry.clone(),
            tools,
        ))
    }
}

/// The chat names of a request's tools, both ways.
#[derive(Debug, Clone, Default)]
pub(crate) struct ToolNames {
    by_wire: HashMap<String, ToolTarget>,
    by_tool: HashMap<(Option<String>, String), String>,
}

/// The tool a chat function name stands for.
#[derive(Debug, Clone, PartialEq)]
struct ToolTarget {
    namespace: Option<String>,
    name: String,
    /// Freeform: its one argument, `input`, is the call's input.
    freeform: bool,
}

impl ToolNames {
    /// Names `name` (in `namespace`) for the chat request, unique within it.
    fn add(&mut self, namespace: Option<&str>, name: &str, freeform: bool) -> String {
        let key = (namespace.map(str::to_string), name.to_string());
        if let Some(wire) = self.by_tool.get(&key) {
            return wire.clone();
        }
        let base = wire_name(namespace, name);
        let mut wire = base.clone();
        let mut n = 2;
        while self.by_wire.contains_key(&wire) {
            let suffix = format!("_{n}");
            let keep = MAX_TOOL_NAME_LEN.saturating_sub(suffix.len());
            wire = format!("{}{suffix}", &base[..base.len().min(keep)]);
            n += 1;
        }
        self.by_wire.insert(
            wire.clone(),
            ToolTarget {
                namespace: key.0.clone(),
                name: name.to_string(),
                freeform,
            },
        );
        self.by_tool.insert(key, wire.clone());
        wire
    }

    /// The chat name of a call to `name` (in `namespace`).
    fn wire(&self, namespace: Option<&str>, name: &str) -> String {
        self.by_tool
            .get(&(namespace.map(str::to_string), name.to_string()))
            .cloned()
            .unwrap_or_else(|| wire_name(namespace, name))
    }

    /// The tool a chat function name stands for (a name the request did not
    /// give stands for itself).
    fn target(&self, wire: &str) -> ToolTarget {
        self.by_wire.get(wire).cloned().unwrap_or(ToolTarget {
            namespace: None,
            name: wire.to_string(),
            freeform: false,
        })
    }
}

/// `<namespace>__<name>` (`<namespace><name>` when the namespace already ends
/// in `_`), in the characters and length chat APIs take.
fn wire_name(namespace: Option<&str>, name: &str) -> String {
    let joined = match namespace.filter(|namespace| !namespace.is_empty()) {
        Some(namespace) if namespace.ends_with('_') => format!("{namespace}{name}"),
        Some(namespace) => format!("{namespace}__{name}"),
        None => name.to_string(),
    };
    joined
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(MAX_TOOL_NAME_LEN)
        .collect()
}

/// The chat completion body for `request`, and its tools' chat names.
pub(crate) fn chat_request(
    request: &ResponsesApiRequest,
    chat: &ChatOptions,
) -> (Value, ToolNames) {
    let mut names = ToolNames::default();
    let tools = chat_tools(request, &mut names);
    let messages = chat_messages(request, &names);
    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
        body["tool_choice"] = Value::String(request.tool_choice.clone());
        body["parallel_tool_calls"] = Value::Bool(request.parallel_tool_calls);
    }
    if let Some(effort) = request
        .reasoning
        .as_ref()
        .and_then(|reasoning| serde_json::to_value(reasoning).ok())
        .and_then(|reasoning| reasoning.get("effort").cloned())
    {
        body["reasoning_effort"] = effort;
    }
    if let Some(format) = request.text.as_ref().and_then(|text| text.format.as_ref()) {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {
                "name": format.name,
                "schema": format.schema,
                // Not strict: chat providers that take a schema at all
                // rarely take a strict one.
                "strict": false,
            },
        });
    }
    if let Value::Object(fields) = &mut body {
        for (key, value) in &chat.extra_body {
            if value.is_null() {
                fields.remove(key);
            } else {
                fields.insert(key.clone(), value.clone());
            }
        }
        for key in &chat.remove {
            fields.remove(key);
        }
    }
    (body, names)
}

/// The request's tools (and those of `AdditionalTools` items) as chat
/// functions.
fn chat_tools(request: &ResponsesApiRequest, names: &mut ToolNames) -> Vec<Value> {
    let mut specs: Vec<Value> = request
        .tools
        .as_ref()
        .and_then(|tools| serde_json::from_str(tools.as_raw_value().get()).ok())
        .unwrap_or_default();
    for item in &request.input {
        if let ResponseItem::AdditionalTools { tools, .. } = item {
            specs.extend(tools.iter().cloned());
        }
    }
    let mut out = Vec::new();
    for spec in &specs {
        push_chat_tool(spec, None, names, &mut out);
    }
    out
}

fn push_chat_tool(
    spec: &Value,
    namespace: Option<&str>,
    names: &mut ToolNames,
    out: &mut Vec<Value>,
) {
    let text = |key: &str| spec.get(key).and_then(Value::as_str).unwrap_or_default();
    match text("type") {
        "function" if !text("name").is_empty() => {
            let name = names.add(namespace, text("name"), /*freeform*/ false);
            let parameters = spec
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            out.push(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": text("description"),
                    "parameters": parameters,
                },
            }));
        }
        "namespace" => {
            let namespace = text("name");
            for tool in spec
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                push_chat_tool(tool, Some(namespace), names, out);
            }
        }
        "custom" if !text("name").is_empty() => {
            let name = names.add(namespace, text("name"), /*freeform*/ true);
            let format = spec.get("format");
            let format_text = |key: &str| {
                format
                    .and_then(|format| format.get(key))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            };
            let mut input = String::from("The tool's input, as plain text.");
            if !format_text("definition").is_empty() {
                input = format!(
                    "The tool's input, as plain text in this {} {} format:\n{}",
                    format_text("syntax"),
                    format_text("type"),
                    format_text("definition")
                );
            }
            out.push(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": text("description"),
                    "parameters": {
                        "type": "object",
                        "properties": {"input": {"type": "string", "description": input}},
                        "required": ["input"],
                        "additionalProperties": false,
                    },
                },
            }));
        }
        // Hosted tools (web search, tool search) have no chat form.
        _ => {}
    }
}

/// One assistant message being gathered: a step's reasoning, text and calls.
#[derive(Default)]
struct AssistantStep {
    content: String,
    reasoning: String,
    tool_calls: Vec<Value>,
}

/// The chat messages of `request`'s instructions and input.
fn chat_messages(request: &ResponsesApiRequest, names: &ToolNames) -> Vec<Value> {
    let mut messages = Vec::new();
    if !request.instructions.is_empty() {
        messages.push(json!({"role": "system", "content": request.instructions}));
    }
    let mut step = AssistantStep::default();
    // Calls of the last assistant message without an output yet.
    let mut open: Vec<String> = Vec::new();
    for item in &request.input {
        match item {
            ResponseItem::Message { role, content, .. } if role == "assistant" => {
                push_line(&mut step.content, &message_text(content));
            }
            ResponseItem::Message { role, content, .. } => {
                flush_step(&mut messages, &mut step, &mut open);
                close_calls(&mut messages, &mut open);
                let content = match role.as_str() {
                    "system" | "developer" => system_content(content),
                    _ => message_content(content),
                };
                messages.push(json!({"role": "user", "content": content}));
            }
            ResponseItem::Reasoning {
                summary, content, ..
            } => {
                // Reasoning after text or calls starts another step.
                if !step.content.is_empty() || !step.tool_calls.is_empty() {
                    flush_step(&mut messages, &mut step, &mut open);
                }
                push_line(
                    &mut step.reasoning,
                    &reasoning_text(summary, content.as_deref()),
                );
            }
            ResponseItem::FunctionCall {
                name,
                namespace,
                arguments,
                call_id,
                ..
            } => {
                let arguments = if arguments.trim().is_empty() {
                    "{}"
                } else {
                    arguments.as_str()
                };
                step.tool_calls.push(tool_call(
                    call_id,
                    &names.wire(namespace.as_deref(), name),
                    arguments,
                ));
            }
            ResponseItem::CustomToolCall {
                call_id,
                name,
                namespace,
                input,
                ..
            } => {
                step.tool_calls.push(tool_call(
                    call_id,
                    &names.wire(namespace.as_deref(), name),
                    &json!({"input": input}).to_string(),
                ));
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                let text = output.body.to_text().unwrap_or_default();
                push_tool_output(
                    &mut messages,
                    &mut step,
                    &mut open,
                    call_id.as_deref(),
                    text,
                );
            }
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                let text = output.body.to_text().unwrap_or_default();
                push_tool_output(&mut messages, &mut step, &mut open, Some(call_id), text);
            }
            ResponseItem::Compaction { .. } | ResponseItem::ContextCompaction { .. } => {
                debug!("a server-side compaction item has no chat form; left out");
            }
            // Items with no chat form (hosted tool calls, request controls).
            _ => {}
        }
    }
    flush_step(&mut messages, &mut step, &mut open);
    close_calls(&mut messages, &mut open);
    messages
}

/// Appends `text` to `out`, on a line of its own.
fn push_line(out: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(text);
}

fn tool_call(call_id: &str, name: &str, arguments: &str) -> Value {
    json!({
        "id": call_id,
        "type": "function",
        "function": {"name": name, "arguments": arguments},
    })
}

/// Sends the step gathered so far as an assistant message (reasoning alone,
/// with nothing said or called, is dropped).
fn flush_step(messages: &mut Vec<Value>, step: &mut AssistantStep, open: &mut Vec<String>) {
    let step = std::mem::take(step);
    if step.content.is_empty() && step.tool_calls.is_empty() {
        return;
    }
    close_calls(messages, open);
    let mut message = json!({
        "role": "assistant",
        "content": if step.content.is_empty() {
            Value::Null
        } else {
            Value::String(step.content)
        },
    });
    if !step.reasoning.is_empty() {
        message["reasoning_content"] = Value::String(step.reasoning);
    }
    if !step.tool_calls.is_empty() {
        open.extend(
            step.tool_calls
                .iter()
                .filter_map(|call| call.get("id").and_then(Value::as_str))
                .map(str::to_string),
        );
        message["tool_calls"] = Value::Array(step.tool_calls);
    }
    messages.push(message);
}

/// Answers the calls still open (a chat history answers every call before it
/// goes on).
fn close_calls(messages: &mut Vec<Value>, open: &mut Vec<String>) {
    for call_id in open.drain(..) {
        messages.push(json!({"role": "tool", "tool_call_id": call_id, "content": ABORTED_OUTPUT}));
    }
}

fn push_tool_output(
    messages: &mut Vec<Value>,
    step: &mut AssistantStep,
    open: &mut Vec<String>,
    call_id: Option<&str>,
    text: String,
) {
    flush_step(messages, step, open);
    match call_id.and_then(|id| open.iter().position(|open_id| open_id == id)) {
        Some(at) => {
            let call_id = open.remove(at);
            messages.push(json!({"role": "tool", "tool_call_id": call_id, "content": text}));
        }
        None => {
            close_calls(messages, open);
            messages.push(json!({"role": "user", "content": format!("(Tool output)\n{text}")}));
        }
    }
}

fn message_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A developer (or system) message of the history: a user message, its text
/// in a `<system>` tag. Only the instructions are a `system` message: one
/// put mid-conversation may be moved to the front by the provider, which
/// breaks its prefix cache, and some providers refuse it.
fn system_content(content: &[ContentItem]) -> Value {
    match message_content(content) {
        Value::String(text) => Value::String(format!(
            "<system>
{text}
</system>"
        )),
        Value::Array(mut parts) => {
            parts.insert(0, json!({"type": "text", "text": "<system>"}));
            parts.push(json!({"type": "text", "text": "</system>"}));
            Value::Array(parts)
        }
        other => other,
    }
}

/// Text, or text and images as content parts (an uploaded image as a
/// `file` part, DeepSeek's form).
fn message_content(content: &[ContentItem]) -> Value {
    let has_image = content
        .iter()
        .any(|item| matches!(item, ContentItem::InputImage { .. }));
    if !has_image {
        return Value::String(message_text(content));
    }
    let parts = content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(json!({"type": "text", "text": text}))
            }
            ContentItem::InputImage {
                image: ImageReference::Inline { image_url },
                ..
            } => Some(json!({"type": "image_url", "image_url": {"url": image_url}})),
            ContentItem::InputImage {
                image: ImageReference::File { file_id },
                ..
            } => Some(json!({"type": "file", "file_id": file_id})),
            _ => None,
        })
        .collect();
    Value::Array(parts)
}

fn reasoning_text(
    summary: &[ReasoningItemReasoningSummary],
    content: Option<&[ReasoningItemContent]>,
) -> String {
    match content.filter(|content| !content.is_empty()) {
        Some(content) => content
            .iter()
            .map(|item| match item {
                ReasoningItemContent::ReasoningText { text }
                | ReasoningItemContent::Text { text } => text.as_str(),
            })
            .collect(),
        None => summary
            .iter()
            .map(|ReasoningItemReasoningSummary::SummaryText { text }| text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn spawn_chat_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    tools: ToolNames,
) -> ResponseStream {
    let upstream_request_id = stream_response
        .headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(async move {
        process_chat_sse(
            stream_response.bytes,
            tx_event,
            idle_timeout,
            telemetry,
            tools,
        )
        .await;
    });
    ResponseStream {
        rx_event,
        upstream_request_id,
    }
}

/// Turns a chat completion stream into [`ResponseEvent`]s: reasoning and text
/// as they come, the tool calls (gathered across chunks) at the end, then
/// `Completed` with the token usage (which comes in a last, choice-less chunk
/// once `stream_options.include_usage` is set).
pub(crate) async fn process_chat_sse<S>(
    stream: S,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    tools: ToolNames,
) where
    S: Stream<Item = Result<bytes::Bytes, codex_client::TransportError>> + Unpin,
{
    let mut stream = stream.eventsource();
    let mut state = ChatStream::new(tools);
    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(telemetry) = telemetry.as_ref() {
            telemetry.on_sse_poll(&response, start.elapsed());
        }
        let event = match response {
            Ok(Some(Ok(event))) => event,
            Ok(Some(Err(err))) => {
                let _ = tx_event.send(Err(ApiError::Stream(err.to_string()))).await;
                return;
            }
            Ok(None) => {
                // Closed without `[DONE]`: done if the choice finished.
                if state.finish_reason.is_some() {
                    state.finish(&tx_event).await;
                } else {
                    let _ = tx_event
                        .send(Err(ApiError::Stream(
                            "stream closed before the chat completion finished".to_string(),
                        )))
                        .await;
                }
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
                    .await;
                return;
            }
        };
        trace!("chat SSE event: {}", event.data);
        let data = event.data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            state.finish(&tx_event).await;
            return;
        }
        let chunk: Value = match serde_json::from_str(data) {
            Ok(chunk) => chunk,
            Err(err) => {
                debug!("unparsable chat SSE event: {err}, data: {data}");
                continue;
            }
        };
        if let Some(error) = chunk.get("error").filter(|error| !error.is_null()) {
            let _ = tx_event.send(Err(chat_error(error))).await;
            return;
        }
        if state.take(&chunk, &tx_event).await {
            state.finish(&tx_event).await;
            return;
        }
    }
}

/// An error a chat provider sent in its stream.
fn chat_error(error: &Value) -> ApiError {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| error.to_string());
    let lowered = message.to_lowercase();
    if lowered.contains("context length") || lowered.contains("maximum context") {
        ApiError::ContextWindowExceeded
    } else {
        ApiError::Stream(message)
    }
}

#[derive(Debug, Default)]
struct ToolCallState {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

/// A chat completion being received.
struct ChatStream {
    tools: ToolNames,
    response_id: Option<String>,
    /// The reasoning so far, while it is the open item.
    reasoning: Option<String>,
    /// The text so far, once it started.
    message: Option<String>,
    calls: BTreeMap<usize, ToolCallState>,
    index_by_id: HashMap<String, usize>,
    /// The call the pieces at a provider's index go to.
    slot_by_wire_index: HashMap<usize, usize>,
    last_index: Option<usize>,
    finish_reason: Option<String>,
    usage: Option<TokenUsage>,
}

impl ChatStream {
    fn new(tools: ToolNames) -> Self {
        Self {
            tools,
            response_id: None,
            reasoning: None,
            message: None,
            calls: BTreeMap::new(),
            index_by_id: HashMap::new(),
            slot_by_wire_index: HashMap::new(),
            last_index: None,
            finish_reason: None,
            usage: None,
        }
    }

    /// Takes one chunk; true once the completion is over (its choice finished
    /// and the usage came).
    async fn take(
        &mut self,
        chunk: &Value,
        tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    ) -> bool {
        if self.response_id.is_none() {
            let id = chunk
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("chatcmpl-{}", uuid::Uuid::new_v4()));
            self.response_id = Some(id.clone());
            let _ = tx_event
                .send(Ok(ResponseEvent::Created {
                    response_id: Some(id),
                }))
                .await;
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| usage.is_object()) {
            self.usage = Some(token_usage(usage));
        }
        for choice in chunk
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let delta = choice.get("delta").or_else(|| choice.get("message"));
            if let Some(delta) = delta {
                if let Some(text) = reasoning_delta(delta) {
                    self.add_reasoning(text, tx_event).await;
                }
                if let Some(text) = content_delta(delta) {
                    self.add_text(text, tx_event).await;
                }
                if let Some(calls) = delta
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .filter(|calls| !calls.is_empty())
                {
                    self.close_reasoning(tx_event).await;
                    for call in calls {
                        self.add_call(call);
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.finish_reason = Some(reason.to_string());
            }
        }
        self.finish_reason.is_some() && self.usage.is_some()
    }

    async fn add_reasoning(
        &mut self,
        text: String,
        tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    ) {
        if text.is_empty() || self.message.is_some() {
            // Reasoning after the text began has no item to go to.
            return;
        }
        if self.reasoning.is_none() {
            self.reasoning = Some(String::new());
            let _ = tx_event
                .send(Ok(ResponseEvent::OutputItemAdded(reasoning_item(
                    String::new(),
                ))))
                .await;
        }
        if let Some(reasoning) = self.reasoning.as_mut() {
            reasoning.push_str(&text);
        }
        let _ = tx_event
            .send(Ok(ResponseEvent::ReasoningContentDelta {
                delta: text,
                content_index: 0,
            }))
            .await;
    }

    async fn add_text(
        &mut self,
        text: String,
        tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    ) {
        if text.is_empty() {
            return;
        }
        self.close_reasoning(tx_event).await;
        if self.message.is_none() {
            self.message = Some(String::new());
            let _ = tx_event
                .send(Ok(ResponseEvent::OutputItemAdded(message_item(
                    String::new(),
                ))))
                .await;
        }
        if let Some(message) = self.message.as_mut() {
            message.push_str(&text);
        }
        let _ = tx_event
            .send(Ok(ResponseEvent::OutputTextDelta(text)))
            .await;
    }

    /// Ends the reasoning item (text or calls follow it).
    async fn close_reasoning(&mut self, tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>) {
        if let Some(reasoning) = self.reasoning.take() {
            let _ = tx_event
                .send(Ok(ResponseEvent::OutputItemDone(reasoning_item(reasoning))))
                .await;
        }
    }

    /// Gathers a piece of a tool call: by its index, else its id, else it
    /// continues the last one.
    fn add_call(&mut self, call: &Value) {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let next = self.calls.keys().next_back().map_or(0, |last| last + 1);
        let wire_index = call
            .get("index")
            .and_then(Value::as_u64)
            .map(|index| index as usize);
        let index = match id.and_then(|id| self.index_by_id.get(id).copied()) {
            Some(known) => known,
            None => match wire_index.and_then(|wire| self.slot_by_wire_index.get(&wire).copied()) {
                // A new id at an index another call holds: a call of its own
                // (some providers give every parallel call index 0).
                Some(slot)
                    if id.is_some()
                        && self.calls.get(&slot).is_some_and(|state| {
                            state.id.as_deref().is_some_and(|known| Some(known) != id)
                        }) =>
                {
                    next
                }
                Some(slot) => slot,
                None if wire_index.is_some() || id.is_some() => next,
                None => self.last_index.unwrap_or(next),
            },
        };
        // Later pieces at this index (without the id) go on with this call.
        if let Some(wire) = wire_index {
            self.slot_by_wire_index.insert(wire, index);
        }
        let state = self.calls.entry(index).or_default();
        if let Some(id) = id {
            state.id.get_or_insert_with(|| id.to_string());
            self.index_by_id.insert(id.to_string(), index);
        }
        if let Some(function) = call.get("function") {
            if let Some(name) = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
            {
                state.name.get_or_insert_with(|| name.to_string());
            }
            if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                state.arguments.push_str(arguments);
            }
        }
        self.last_index = Some(index);
    }

    /// Ends the completion: its items, then `Completed`. Cut off at the
    /// output token limit with tool calls (their arguments unfinished) or
    /// with nothing said (only reasoning), it fails.
    async fn finish(&mut self, tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>) {
        if self.finish_reason.as_deref() == Some("length")
            && (!self.calls.is_empty() || self.message.is_none())
        {
            let _ = tx_event
                .send(Err(ApiError::Stream(
                    "the response was cut off at the output token limit".to_string(),
                )))
                .await;
            return;
        }
        self.close_reasoning(tx_event).await;
        if let Some(message) = self.message.take() {
            let _ = tx_event
                .send(Ok(ResponseEvent::OutputItemDone(message_item(message))))
                .await;
        }
        let response_id = self
            .response_id
            .clone()
            .unwrap_or_else(|| format!("chatcmpl-{}", uuid::Uuid::new_v4()));
        for (index, call) in std::mem::take(&mut self.calls) {
            let Some(name) = call.name else {
                debug!("chat tool call {index} has no name; skipped");
                continue;
            };
            let call_id = call
                .id
                .unwrap_or_else(|| format!("call_{response_id}_{index}"));
            let target = self.tools.target(&name);
            let item = if target.freeform {
                let input = serde_json::from_str::<Value>(&call.arguments)
                    .ok()
                    .and_then(|arguments| {
                        arguments
                            .get("input")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or(call.arguments);
                ResponseItem::CustomToolCall {
                    id: None,
                    status: None,
                    call_id,
                    name: target.name,
                    namespace: target.namespace,
                    input,
                    internal_chat_message_metadata_passthrough: None,
                }
            } else {
                ResponseItem::FunctionCall {
                    id: None,
                    name: target.name,
                    namespace: target.namespace,
                    arguments: if call.arguments.trim().is_empty() {
                        "{}".to_string()
                    } else {
                        call.arguments
                    },
                    encrypted_function_args: None,
                    call_id,
                    internal_chat_message_metadata_passthrough: None,
                }
            };
            let _ = tx_event.send(Ok(ResponseEvent::OutputItemDone(item))).await;
        }
        let _ = tx_event
            .send(Ok(ResponseEvent::Completed {
                response_id,
                token_usage: self.usage.take(),
                usage_metadata: None,
                end_turn: None,
            }))
            .await;
    }
}

/// Reasoning text of a delta: `reasoning_content` (DeepSeek and others) or
/// `reasoning` (a string, or an object with `text` / `content`).
fn reasoning_delta(delta: &Value) -> Option<String> {
    if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    let reasoning = delta.get("reasoning")?;
    reasoning
        .as_str()
        .or_else(|| reasoning.get("text").and_then(Value::as_str))
        .or_else(|| reasoning.get("content").and_then(Value::as_str))
        .map(str::to_string)
}

/// Text of a delta's `content` (a string, or parts with `text`).
fn content_delta(delta: &Value) -> Option<String> {
    match delta.get("content")? {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect(),
        ),
        _ => None,
    }
}

fn message_item(text: String) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: if text.is_empty() {
            Vec::new()
        } else {
            vec![ContentItem::OutputText { text }]
        },
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn reasoning_item(text: String) -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: Vec::new(),
        content: Some(if text.is_empty() {
            Vec::new()
        } else {
            vec![ReasoningItemContent::ReasoningText { text }]
        }),
        encrypted_content: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

/// Token counts of a chat `usage`: cached input from
/// `prompt_tokens_details.cached_tokens` or DeepSeek's
/// `prompt_cache_hit_tokens`.
fn token_usage(usage: &Value) -> TokenUsage {
    let count = |pointer: &str| usage.pointer(pointer).and_then(Value::as_i64);
    let input_tokens = count("/prompt_tokens").unwrap_or(0);
    let output_tokens = count("/completion_tokens").unwrap_or(0);
    TokenUsage {
        input_tokens,
        cached_input_tokens: count("/prompt_tokens_details/cached_tokens")
            .or_else(|| count("/prompt_cache_hit_tokens"))
            .unwrap_or(0),
        cache_write_input_tokens: 0,
        output_tokens,
        reasoning_output_tokens: count("/completion_tokens_details/reasoning_tokens").unwrap_or(0),
        total_tokens: count("/total_tokens").unwrap_or(input_tokens + output_tokens),
        codex_rollout_budget_units: None,
    }
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
