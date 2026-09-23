use std::collections::BTreeMap;
use std::collections::HashSet;

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

    /// Fork addition: the stored values `history` holds, rendered again for a
    /// history that will not (compaction), with their keys. A value submitted
    /// but not recorded yet is left out: its own fragment is on the way.
    pub(crate) fn render_recorded<'a>(
        &self,
        max_tokens: usize,
        history: impl IntoIterator<Item = &'a ResponseItem>,
    ) -> Vec<(String, ResponseItem)> {
        let recorded = history
            .into_iter()
            .filter_map(message_text)
            .collect::<HashSet<_>>();
        self.values
            .iter()
            .map(|(key, entry)| (key.clone(), fragment(key, entry, max_tokens)))
            .filter(|(_, item)| message_text(item).is_some_and(|text| recorded.contains(&text)))
            .collect()
    }
}

/// Fork addition: whether `item` is a fragment for `key`, whatever its value.
pub(crate) fn is_fragment_of(item: &ResponseItem, key: &str) -> bool {
    match message_text(item) {
        Some(("developer", text)) => {
            text.starts_with(&format!("<{key}>")) && text.ends_with(&format!("</{key}>"))
        }
        Some(("user", text)) => text.starts_with(&format!("<external_{key}>")),
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
