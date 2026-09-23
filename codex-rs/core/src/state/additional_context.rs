use std::collections::BTreeMap;

use crate::context::AdditionalContextDeveloperFragment;
use crate::context::AdditionalContextUserFragment;
use crate::context::ContextualUserFragment;
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

    /// Fork addition: every stored value as it would be sent, for history
    /// that no longer holds them (after compaction).
    pub(crate) fn render_all(&self, max_tokens: usize) -> Vec<ResponseItem> {
        self.values
            .iter()
            .map(|(key, entry)| fragment(key, entry, max_tokens))
            .collect()
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
