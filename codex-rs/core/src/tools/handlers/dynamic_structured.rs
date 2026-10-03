//! Structured code-mode results for dynamic tools (fork addition).
//!
//! Upstream returns a dynamic tool's output to code-mode scripts as one joined
//! string, which drops `success` and turns images into bare data URLs. When
//! `features.code_mode.structured_dynamic_tool_results` is enabled, scripts
//! instead receive an MCP-shaped object:
//!
//! ```json
//! {"content": [{"type": "text", "text": "..."},
//!              {"type": "text", "text": "[image shown to you in this output]"}],
//!  "isError": false,
//!  "text": "joined text items"}
//! ```
//!
//! so failures are visible. An inline image the tool returned goes to the
//! model directly, as `image()` would show it (the code-mode runtime takes it
//! from `ATTACH_IMAGES_KEY`), and the script gets a short note in its place:
//! never the base64 text, which a script printing its result would otherwise
//! dump into the output. Model-facing output for direct calls is unchanged.

use codex_code_mode::ATTACH_IMAGES_KEY;
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
    let mut images = Vec::new();
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
                Some(_) => {
                    images.push(image_url.clone());
                    content.push(json!({"type": "text", "text": IMAGE_SHOWN_NOTE}));
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
    let mut result = json!({
        "content": content,
        "isError": !success.unwrap_or(true),
        "text": texts.join("\n"),
    });
    if !images.is_empty() {
        result[ATTACH_IMAGES_KEY] = json!(images);
    }
    result
}

/// What a script finds in place of an image the model was shown.
const IMAGE_SHOWN_NOTE: &str = "[image shown to you in this output]";

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
                    {"type": "text", "text": "[image shown to you in this output]"},
                    {"type": "text", "text": "world"},
                ],
                "isError": true,
                "text": "hello\nworld",
                "__codex_attach_images": ["data:image/png;base64,AAAA"],
            })
        );
    }
}
