//! AstrBot: what a provider's `open_model` option changes in the history an
//! open-weight model is sent (see `codex_config::model_provider_options`).
//!
//! A thread that an OpenAI model worked on holds content only OpenAI can
//! read: encrypted reasoning, remote compaction summaries, encrypted parts of
//! messages and tool outputs. An open model cannot use it (and a provider may
//! refuse it), so its requests leave it out, and the first turn after such a
//! switch tells the model that some earlier context is missing.

use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;

/// Stands in for an encrypted part of a message or tool output.
pub(crate) const ENCRYPTED_CONTENT_PLACEHOLDER: &str =
    "[encrypted content from a previous model omitted]";

/// Whether `item` carries content encrypted by OpenAI that an open model
/// cannot read: encrypted reasoning, a compaction summary, or an encrypted
/// part of an agent message or a tool output. (A top-level
/// `encrypted_content` beside readable content, as on messages and calls,
/// loses nothing when cleared, so it does not count.)
pub(crate) fn has_openai_encrypted_content(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Reasoning {
            encrypted_content, ..
        }
        | ResponseItem::ContextCompaction {
            encrypted_content, ..
        } => encrypted_content.is_some(),
        ResponseItem::Compaction { .. } => true,
        ResponseItem::AgentMessage { content, .. } => content
            .iter()
            .any(|part| matches!(part, AgentMessageInputContent::EncryptedContent { .. })),
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => has_encrypted_parts(output),
        _ => false,
    }
}

fn has_encrypted_parts(output: &FunctionCallOutputPayload) -> bool {
    output.content_items().is_some_and(|parts| {
        parts
            .iter()
            .any(|part| matches!(part, FunctionCallOutputContentItem::EncryptedContent { .. }))
    })
}

/// Leaves OpenAI's encrypted content out of a request for an open model:
/// encrypted reasoning and compaction items are dropped (the model's own
/// reasoning has none and stays), every other encrypted field is cleared, and
/// an encrypted part of an agent message or tool output becomes a short
/// placeholder (a call keeps its output).
pub(crate) fn strip_openai_encrypted_content(input: &mut Vec<ResponseItem>) {
    input.retain(|item| match item {
        ResponseItem::Reasoning {
            encrypted_content, ..
        }
        | ResponseItem::ContextCompaction {
            encrypted_content, ..
        } => encrypted_content.is_none(),
        ResponseItem::Compaction { .. } => false,
        _ => true,
    });
    for item in input.iter_mut() {
        item.clear_encrypted_content();
        match item {
            ResponseItem::FunctionCall {
                encrypted_function_args,
                ..
            } => *encrypted_function_args = None,
            ResponseItem::AgentMessage { content, .. } => {
                for part in content.iter_mut() {
                    if matches!(part, AgentMessageInputContent::EncryptedContent { .. }) {
                        *part = AgentMessageInputContent::InputText {
                            text: ENCRYPTED_CONTENT_PLACEHOLDER.to_string(),
                        };
                    }
                }
            }
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => {
                if let Some(parts) = output.content_items_mut() {
                    for part in parts.iter_mut() {
                        if matches!(part, FunctionCallOutputContentItem::EncryptedContent { .. }) {
                            *part = FunctionCallOutputContentItem::InputText {
                                text: ENCRYPTED_CONTENT_PLACEHOLDER.to_string(),
                            };
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "open_model_tests.rs"]
mod tests;
