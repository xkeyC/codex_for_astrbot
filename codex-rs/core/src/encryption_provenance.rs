//! AstrBot: encrypted history belongs to the provider and account that made it.
//!
//! Encrypted content in history (reasoning, compaction summaries, encrypted
//! parts of messages and tool outputs) can be read only by the identity that
//! produced it: a model provider id together with the account it was used
//! with (the ChatGPT account, or a short hash of the API key or bearer token).
//! Every item with encrypted content is recorded with its owner
//! (`CodexHarnessMetadata::encryption_owner`, persisted in the rollout and
//! kept through compaction). Requests send the current identity's items back
//! unchanged and leave out what other identities encrypted; the first turn
//! after such a switch tells the model once that some earlier context is
//! missing.
//!
//! History written before owners were kept is taken as the thread's
//! `SessionMeta` provider's with an unknown account: it is foreign to another
//! provider id, and kept for the same one (an account change cannot be told).

use codex_history::CodexHarnessMetadata;
use codex_history::EncryptionOwner;
use codex_history::ResponseItemEnvelope;
use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use sha1::Digest;

use crate::session::turn_context::TurnContext;

/// Stands in for an encrypted part of a message or tool output.
pub(crate) const ENCRYPTED_CONTENT_PLACEHOLDER: &str =
    "[encrypted content from a previous model omitted]";

/// Hex digits of the key hash kept as the account.
const KEY_HASH_LEN: usize = 16;

/// Whose encrypted content a session's requests may carry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EncryptionProvenance {
    /// The identity requests go to now; set when a task starts. Until then
    /// nothing is foreign and nothing is marked.
    pub(crate) current: Option<EncryptionOwner>,
    /// The `SessionMeta` provider of resumed or forked history: the owner of
    /// its items that have no mark.
    pub(crate) legacy_provider: Option<String>,
}

impl EncryptionProvenance {
    /// Whether `envelope` was produced under another identity than the
    /// current one.
    pub(crate) fn is_foreign(&self, envelope: &ResponseItemEnvelope) -> bool {
        let Some(current) = &self.current else {
            return false;
        };
        match owner(envelope) {
            Some(owner) => owner != current,
            None => self
                .legacy_provider
                .as_ref()
                .is_some_and(|provider| provider != &current.provider),
        }
    }

    /// Whether `envelope` is foreign and holds encrypted content the current
    /// identity cannot read (see [`has_unreadable_encrypted_content`]).
    pub(crate) fn has_foreign_encrypted_content(&self, envelope: &ResponseItemEnvelope) -> bool {
        has_unreadable_encrypted_content(&envelope.item) && self.is_foreign(envelope)
    }

    /// Marks the items with encrypted content as the current identity's.
    /// An item that already has an owner keeps it.
    pub(crate) fn mark_produced<'a>(
        &self,
        envelopes: impl IntoIterator<Item = &'a mut ResponseItemEnvelope>,
    ) {
        let Some(current) = &self.current else {
            return;
        };
        for envelope in envelopes {
            if owner(envelope).is_none() && has_encrypted_content(&envelope.item) {
                set_owner(&mut envelope.metadata, current.clone());
            }
        }
    }

    /// Leaves out of `items` (a request's history) the encrypted content of
    /// other identities: their encrypted reasoning and compaction items are
    /// dropped, their other encrypted fields cleared, and an encrypted part of
    /// their agent messages or tool outputs becomes a short placeholder (a
    /// call keeps its output). The current identity's items stay as they are.
    pub(crate) fn strip_foreign(&self, items: &mut Vec<ResponseItemEnvelope>) {
        if self.current.is_none() {
            return;
        }
        items.retain(|envelope| {
            !self.is_foreign(envelope)
                || match &envelope.item {
                    ResponseItem::Reasoning {
                        encrypted_content, ..
                    }
                    | ResponseItem::ContextCompaction {
                        encrypted_content, ..
                    } => encrypted_content.is_none(),
                    ResponseItem::Compaction { .. } => false,
                    _ => true,
                }
        });
        for envelope in items.iter_mut() {
            if self.is_foreign(envelope) {
                strip_item(&mut envelope.item);
            }
        }
    }
}

/// The owner recorded for `envelope`, if any.
pub(crate) fn owner(envelope: &ResponseItemEnvelope) -> Option<&EncryptionOwner> {
    envelope.metadata.as_ref()?.encryption_owner.as_deref()
}

pub(crate) fn set_owner(metadata: &mut Option<CodexHarnessMetadata>, owner: EncryptionOwner) {
    metadata.get_or_insert_default().encryption_owner = Some(Box::new(owner));
}

/// The identity this turn's requests go to: its provider id and account.
pub(crate) async fn current_owner(turn_context: &TurnContext) -> EncryptionOwner {
    let provider = &turn_context.provider;
    let account = match provider.auth().await {
        Some(auth) if auth.is_chatgpt_auth() => auth
            .get_account_id()
            .map(|account_id| format!("chatgpt:{account_id}")),
        _ => match provider.api_auth().await {
            Ok(auth) => auth
                .to_auth_headers()
                .ok()
                .and_then(|headers| key_hash(&headers)),
            Err(_) => None,
        },
    };
    EncryptionOwner {
        provider: turn_context.config.model_provider_id.clone(),
        account,
    }
}

/// `key:<hash>` of the credential headers a provider sends, never the secret.
fn key_hash(headers: &http::HeaderMap) -> Option<String> {
    let mut credentials = ["authorization", "api-key", "x-api-key"]
        .into_iter()
        .filter_map(|name| Some((name, headers.get(name)?)))
        .peekable();
    credentials.peek()?;
    let mut hasher = sha1::Sha1::new();
    for (name, value) in credentials {
        hasher.update(name.as_bytes());
        hasher.update(b"\0");
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Some(format!("key:{}", &hex[..KEY_HASH_LEN]))
}

/// Whether `item` carries encrypted content another identity cannot read:
/// encrypted reasoning, a compaction summary, or an encrypted part of an agent
/// message or a tool output. (A top-level `encrypted_content` beside readable
/// content, as on messages and calls, loses nothing when cleared, so it does
/// not count.)
pub(crate) fn has_unreadable_encrypted_content(item: &ResponseItem) -> bool {
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

/// Whether `item` carries any encrypted content at all (its owner is kept).
fn has_encrypted_content(item: &ResponseItem) -> bool {
    has_unreadable_encrypted_content(item)
        || match item {
            ResponseItem::Message {
                encrypted_content, ..
            }
            | ResponseItem::LocalShellCall {
                encrypted_content, ..
            }
            | ResponseItem::ToolSearchCall {
                encrypted_content, ..
            }
            | ResponseItem::CustomToolCall {
                encrypted_content, ..
            }
            | ResponseItem::WebSearchCall {
                encrypted_content, ..
            }
            | ResponseItem::ImageGenerationCall {
                encrypted_content, ..
            } => encrypted_content.is_some(),
            ResponseItem::FunctionCall {
                encrypted_content,
                encrypted_function_args,
                ..
            } => encrypted_content.is_some() || encrypted_function_args.is_some(),
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

/// Clears a foreign item's encrypted fields and replaces its encrypted parts.
fn strip_item(item: &mut ResponseItem) {
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

#[cfg(test)]
#[path = "encryption_provenance_tests.rs"]
mod tests;
