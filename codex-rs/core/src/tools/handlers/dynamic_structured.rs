//! Structured code-mode results for dynamic tools (fork addition).
//!
//! Upstream returns a dynamic tool's output to code-mode scripts as one joined
//! string, which drops `success` and turns images into bare data URLs. When
//! `features.code_mode.structured_dynamic_tool_results` is enabled, scripts
//! instead receive an MCP-shaped object:
//!
//! ```json
//! {"content": [{"type": "text", "text": "..."},
//!              {"type": "image", "data": "<base64>", "mimeType": "image/png"}],
//!  "isError": false,
//!  "text": "joined text items"}
//! ```
//!
//! so `image(result.content[i])` works and failures are visible. Model-facing
//! output for direct calls is unchanged.

use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ImageReference;
use codex_protocol::models::ResponseInputItem;
use serde_json::Value as JsonValue;
use serde_json::json;

use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;

pub(crate) struct StructuredDynamicToolOutput {
    inner: FunctionToolOutput,
}

impl StructuredDynamicToolOutput {
    pub(crate) fn new(inner: FunctionToolOutput) -> Self {
        Self { inner }
    }
}

fn split_data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    Some((mime, data))
}

pub(crate) fn structured_code_mode_result(
    body: &[FunctionCallOutputContentItem],
    success: Option<bool>,
) -> JsonValue {
    let mut content = Vec::with_capacity(body.len());
    let mut texts = Vec::new();
    for item in body {
        match item {
            FunctionCallOutputContentItem::InputText { text } => {
                texts.push(text.as_str());
                content.push(json!({"type": "text", "text": text}));
            }
            FunctionCallOutputContentItem::InputImage {
                image: ImageReference::Inline { image_url },
                ..
            } => match split_data_url(image_url) {
                Some((mime, data)) => {
                    content.push(json!({"type": "image", "data": data, "mimeType": mime}));
                }
                None => content.push(json!({"type": "text", "text": image_url})),
            },
            FunctionCallOutputContentItem::InputImage {
                image: ImageReference::File { file_id },
                ..
            } => content.push(json!({"type": "text", "text": format!("[image file {file_id}]")})),
            FunctionCallOutputContentItem::InputAudio { audio_url } => {
                match split_data_url(audio_url) {
                    Some((mime, data)) => {
                        content.push(json!({"type": "audio", "data": data, "mimeType": mime}));
                    }
                    None => content.push(json!({"type": "text", "text": audio_url})),
                }
            }
            FunctionCallOutputContentItem::EncryptedContent { .. } => {}
        }
    }
    json!({
        "content": content,
        "isError": !success.unwrap_or(true),
        "text": texts.join("\n"),
    })
}

impl ToolOutput for StructuredDynamicToolOutput {
    fn log_output(&self) -> String {
        self.inner.log_output()
    }

    fn success_for_logging(&self) -> bool {
        self.inner.success_for_logging()
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        self.inner.to_response_item(call_id, payload)
    }

    fn post_tool_use_response(&self, call_id: &str, payload: &ToolPayload) -> Option<JsonValue> {
        self.inner.post_tool_use_response(call_id, payload)
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        structured_code_mode_result(&self.inner.body, self.inner.success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn keeps_success_text_and_images() {
        let body = vec![
            FunctionCallOutputContentItem::InputText {
                text: "hello".to_string(),
            },
            FunctionCallOutputContentItem::InputImage {
                image: ImageReference::Inline {
                    image_url: "data:image/png;base64,AAAA".to_string(),
                },
                detail: None,
            },
            FunctionCallOutputContentItem::InputText {
                text: "world".to_string(),
            },
        ];
        assert_eq!(
            structured_code_mode_result(&body, Some(false)),
            json!({
                "content": [
                    {"type": "text", "text": "hello"},
                    {"type": "image", "data": "AAAA", "mimeType": "image/png"},
                    {"type": "text", "text": "world"},
                ],
                "isError": true,
                "text": "hello\nworld",
            })
        );
    }
}
