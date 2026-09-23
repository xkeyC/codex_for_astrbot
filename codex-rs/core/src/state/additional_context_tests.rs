use std::collections::BTreeMap;

use codex_protocol::models::ResponseItem;
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

fn keys(fragments: &[(String, ResponseItem)]) -> Vec<&str> {
    fragments.iter().map(|(key, _)| key.as_str()).collect()
}

#[test]
fn each_key_carries_the_last_fragment_history_holds() {
    let mut store = AdditionalContextStore::default();
    let mut history = store.merge(
        values(&[
            ("persona", "be a cat", AdditionalContextKind::Application),
            ("tab", "one", AdditionalContextKind::Untrusted),
        ]),
        /*max_tokens*/ 1_000,
    );
    // The persona changes and is recorded; then it changes again, but that
    // fragment is submitted and not recorded yet.
    history.extend(store.merge(
        values(&[
            ("persona", "be a dog", AdditionalContextKind::Application),
            ("tab", "one", AdditionalContextKind::Untrusted),
        ]),
        /*max_tokens*/ 1_000,
    ));
    let pending = store.merge(
        values(&[
            ("persona", "be a fox", AdditionalContextKind::Application),
            ("tab", "one", AdditionalContextKind::Untrusted),
        ]),
        /*max_tokens*/ 1_000,
    );

    let carried = store.last_fragments(history.iter());
    assert_eq!(keys(&carried), vec!["persona", "tab"]);
    // The dog the model saw last, not the fox still on the way.
    assert_eq!(carried[0].1, history[2]);
    assert_eq!(carried[1].1, history[1]);
    assert!(is_fragment_of(&pending[0], "persona"));
    assert!(!is_fragment_of(&pending[0], "persona_examples"));
    assert!(!is_fragment_of(&pending[0], "tab"));
}

#[test]
fn a_key_history_never_held_is_not_carried() {
    let mut store = AdditionalContextStore::default();
    store.merge(
        values(&[("persona", "be a cat", AdditionalContextKind::Application)]),
        /*max_tokens*/ 1_000,
    );
    assert!(store.last_fragments(std::iter::empty()).is_empty());
}

#[test]
fn values_whose_fragments_were_dropped_are_sent_again() {
    let persona = |value| values(&[("persona", value, AdditionalContextKind::Application)]);
    let mut store = AdditionalContextStore::default();
    store.merge(persona("be a cat"), /*max_tokens*/ 1_000);
    let dropped = store.merge(persona("be a dog"), /*max_tokens*/ 1_000);

    store.forget_unrecorded(dropped.iter());
    assert_eq!(
        store.merge(persona("be a dog"), /*max_tokens*/ 1_000),
        dropped
    );
}
