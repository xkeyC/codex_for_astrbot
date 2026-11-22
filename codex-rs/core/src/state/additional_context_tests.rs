use std::collections::BTreeMap;

use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::protocol::AdditionalContextEntry;
use codex_protocol::protocol::AdditionalContextKind;
use codex_protocol::protocol::SessionSource;
use pretty_assertions::assert_eq;

use super::AdditionalContextFormat;
use super::AdditionalContextStore;
use crate::session::session::Session;

fn telemetry() -> SessionTelemetry {
    SessionTelemetry::new(
        ThreadId::new(),
        "gpt-test",
        "gpt-test",
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        "test-originator".to_string(),
        /*log_user_prompts*/ false,
        "test-terminal".to_string(),
        SessionSource::Cli,
    )
}

fn persona(value: &str) -> BTreeMap<String, AdditionalContextEntry> {
    BTreeMap::from([(
        "persona".to_string(),
        AdditionalContextEntry {
            value: value.to_string(),
            kind: AdditionalContextKind::Application,
            name: None,
        },
    )])
}

#[test]
fn values_whose_items_were_dropped_are_sent_again() {
    let telemetry = telemetry();
    let ids = Session::new_item_id_generator();
    let mut store = AdditionalContextStore::default();
    let merge = |store: &mut AdditionalContextStore, value| {
        store.merge(
            persona(value),
            AdditionalContextFormat::Messages,
            &telemetry,
            &ids,
            /*max_tokens*/ 1_000,
        )
    };
    merge(&mut store, "be a cat");
    let dropped = merge(&mut store, "be a dog");
    assert_eq!(dropped.len(), 1);
    // Unchanged, and still counted as sent: nothing new.
    assert!(merge(&mut store, "be a dog").is_empty());

    store.forget_unrecorded(dropped.iter());
    // Sent again, under a new item ID.
    let without_ids = |items: Vec<codex_history::ResponseItemEnvelope>| {
        items
            .into_iter()
            .map(|mut item| {
                item.set_id(None);
                item.item
            })
            .collect::<Vec<_>>()
    };
    let resent = merge(&mut store, "be a dog");
    assert_eq!(without_ids(resent), without_ids(dropped));
}

#[test]
fn forgetting_other_items_keeps_values_sent() {
    let telemetry = telemetry();
    let ids = Session::new_item_id_generator();
    let mut store = AdditionalContextStore::default();
    let first = store.merge(
        persona("be a cat"),
        AdditionalContextFormat::Messages,
        &telemetry,
        &ids,
        /*max_tokens*/ 1_000,
    );
    let mut other = AdditionalContextStore::default();
    let unrelated = other.merge(
        persona("be a cat"),
        AdditionalContextFormat::Messages,
        &telemetry,
        &ids,
        /*max_tokens*/ 1_000,
    );
    assert_eq!(first.len(), 1);

    store.forget_unrecorded(unrelated.iter());
    assert!(
        store
            .merge(
                persona("be a cat"),
                AdditionalContextFormat::Messages,
                &telemetry,
                &ids,
                /*max_tokens*/ 1_000,
            )
            .is_empty()
    );
}
