use std::collections::BTreeMap;

use crate::context::AdditionalContextDeveloperFragment;
use crate::context::AdditionalContextUserFragment;
use crate::context::ContextualUserFragment;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AdditionalContextEntry;
use codex_protocol::protocol::AdditionalContextKind;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AdditionalContextStore {
    values: BTreeMap<String, AdditionalContextEntry>,
}

impl AdditionalContextStore {
    pub(crate) fn merge(
        &mut self,
        values: BTreeMap<String, AdditionalContextEntry>,
        max_tokens: usize,
    ) -> Vec<ResponseItem> {
        let fragments = values
            .iter()
            .filter(|(key, value)| self.values.get(*key) != Some(*value))
            .map(|(key, entry)| fragment(key, entry, max_tokens))
            .collect();
        self.values = values;
        fragments
    }

    /// Fork addition: for each stored key, the last fragment `history`
    /// holds for it, to carry into a history that will not (compaction). What
    /// the model saw stays as it was: a value submitted but not recorded yet
    /// follows as its own fragment, and a key history lost stays lost.
    pub(crate) fn last_fragments<'a>(
        &self,
        history: impl Clone + DoubleEndedIterator<Item = &'a ResponseItem>,
    ) -> Vec<(String, ResponseItem)> {
        self.values
            .keys()
            .filter_map(|key| {
                history
                    .clone()
                    .rev()
                    .find(|item| is_fragment_of(item, key))
                    .map(|item| (key.clone(), item.clone()))
            })
            .collect()
    }
}

/// Fork addition: whether `item` is a fragment for `key`, whatever its value.
pub(crate) fn is_fragment_of(item: &ResponseItem, key: &str) -> bool {
    if let ResponseItem::Message {
        internal_chat_message_metadata_passthrough: Some(metadata),
        ..
    } = item
        && let Some(kinds) = &metadata.content_item_kinds
        && !kinds
            .iter()
            .any(|kind| kind.0 == format!("additional_content.{key}"))
    {
        return false;
    }
    match message_text(item) {
        Some(("developer", text)) => {
            text.starts_with(&format!("<{key}>")) && text.ends_with(&format!("</{key}>"))
        }
        Some(("user", text)) => {
            text.starts_with(&format!("<external_{key}>"))
                && text.ends_with(&format!("</external_{key}>"))
        }
        Some(_) | None => false,
    }
}

/// Role and text of a single-text message.
fn message_text(item: &ResponseItem) -> Option<(&str, &str)> {
    let ResponseItem::Message { role, content, .. } = item else {
        return None;
    };
    match content.as_slice() {
        [ContentItem::InputText { text }] => Some((role.as_str(), text.as_str())),
        _ => None,
    }
}

fn fragment(key: &str, entry: &AdditionalContextEntry, max_tokens: usize) -> ResponseItem {
    match entry.kind {
        AdditionalContextKind::Untrusted => ContextualUserFragment::into(
            AdditionalContextUserFragment::new(key.to_string(), entry.value.clone())
                .with_max_tokens(max_tokens),
        ),
        AdditionalContextKind::Application => ContextualUserFragment::into(
            AdditionalContextDeveloperFragment::new(key.to_string(), entry.value.clone())
                .with_max_tokens(max_tokens),
        ),
    }
}

#[cfg(test)]
#[path = "additional_context_tests.rs"]
mod tests;
