use std::collections::BTreeMap;

use codex_protocol::protocol::AdditionalContextEntry;
use codex_protocol::protocol::AdditionalContextKind;
use pretty_assertions::assert_eq;

use super::AdditionalContextStore;
use super::is_fragment_of;

fn values(
    pairs: &[(&str, &str, AdditionalContextKind)],
) -> BTreeMap<String, AdditionalContextEntry> {
    pairs
        .iter()
        .map(|(key, value, kind)| {
            (
                (*key).to_string(),
                AdditionalContextEntry {
                    value: (*value).to_string(),
                    kind: *kind,
                },
            )
        })
        .collect()
}

#[test]
fn only_values_history_holds_are_rendered_again() {
    let mut store = AdditionalContextStore::default();
    let recorded = store.merge(
        values(&[
            ("persona", "be a cat", AdditionalContextKind::Application),
            ("tab", "one", AdditionalContextKind::Untrusted),
        ]),
        /*max_tokens*/ 1_000,
    );
    // The persona changes; its new fragment is submitted but not recorded.
    let pending = store.merge(
        values(&[
            ("persona", "be a dog", AdditionalContextKind::Application),
            ("tab", "one", AdditionalContextKind::Untrusted),
        ]),
        /*max_tokens*/ 1_000,
    );
    assert_eq!(pending.len(), 1);

    let again = store.render_recorded(/*max_tokens*/ 1_000, &recorded);
    assert_eq!(
        again
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        vec!["tab"]
    );
    assert!(is_fragment_of(&again[0].1, "tab"));
    assert!(is_fragment_of(&recorded[0], "persona"));
    assert!(is_fragment_of(&pending[0], "persona"));
    assert!(!is_fragment_of(&pending[0], "persona_examples"));
}
