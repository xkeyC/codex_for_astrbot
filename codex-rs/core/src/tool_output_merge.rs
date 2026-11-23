//! AstrBot: one output per call in a request.
//!
//! Code mode's `notify()` records an extra output for the `exec` (or
//! `run_code`) call while its program runs. OpenAI accepts several
//! `custom_tool_call_output` items for one call, which is upstream's design;
//! other providers refuse a second output for a call id (DeepSeek: "Duplicate
//! tool output"). So a request merges the outputs of a call into its first
//! one, at that one's place. History and rollout keep them apart.
//!
//! The notifications are recorded after the program's output (they wait in
//! the turn's pending input until the next request) although they were sent
//! while it ran: before a final result they read first, after a
//! `Script running ...` (yielded) output they read after it.

use std::collections::HashMap;

use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;

const YIELDED_PREFIX: &str = "Script running with cell ID ";

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Function,
    Custom,
}

/// Merges the outputs sharing a call id into the first of them; custom tool
/// outputs only when `custom` is set (function outputs always).
pub(crate) fn merge_duplicate_tool_outputs(input: &mut Vec<ResponseItem>, custom: bool) {
    let mut counts = HashMap::<(Kind, &str), usize>::new();
    for item in input.iter() {
        if let Some(key) = output_key(item, custom) {
            *counts.entry(key).or_default() += 1;
        }
    }
    if counts.values().all(|count| *count < 2) {
        return;
    }
    let duplicated = counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|((kind, call_id), _)| (kind, call_id.to_string()))
        .collect::<std::collections::HashSet<_>>();

    // The first output of each duplicated call, and the later ones' payloads.
    let mut first_at = HashMap::<(Kind, String), usize>::new();
    let mut extras = HashMap::<(Kind, String), Vec<FunctionCallOutputPayload>>::new();
    let mut kept = Vec::with_capacity(input.len());
    for item in input.drain(..) {
        let key = output_key(&item, custom)
            .map(|(kind, call_id)| (kind, call_id.to_string()))
            .filter(|key| duplicated.contains(key));
        match key {
            Some(key) if first_at.contains_key(&key) => {
                if let Some(output) = into_output(item) {
                    extras.entry(key).or_default().push(output);
                }
            }
            Some(key) => {
                first_at.insert(key, kept.len());
                kept.push(item);
            }
            None => kept.push(item),
        }
    }
    for (key, later) in extras {
        if let Some(output) = first_at.get(&key).and_then(|at| output_mut(&mut kept[*at])) {
            merge_into(output, later);
        }
    }
    *input = kept;
}

fn output_key(item: &ResponseItem, custom: bool) -> Option<(Kind, &str)> {
    match item {
        ResponseItem::FunctionCallOutput {
            call_id: Some(call_id),
            ..
        } => Some((Kind::Function, call_id.as_str())),
        ResponseItem::CustomToolCallOutput { call_id, .. } if custom => {
            Some((Kind::Custom, call_id.as_str()))
        }
        _ => None,
    }
}

fn into_output(item: ResponseItem) -> Option<FunctionCallOutputPayload> {
    match item {
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => Some(output),
        _ => None,
    }
}

fn output_mut(item: &mut ResponseItem) -> Option<&mut FunctionCallOutputPayload> {
    match item {
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => Some(output),
        _ => None,
    }
}

fn parts(body: FunctionCallOutputBody) -> Vec<FunctionCallOutputContentItem> {
    match body {
        FunctionCallOutputBody::Text(text) => {
            vec![FunctionCallOutputContentItem::InputText { text }]
        }
        FunctionCallOutputBody::ContentItems(items) => items,
    }
}

/// Adds `later` outputs to `output`: before its content when it is a final
/// result, after it when the program had only yielded.
fn merge_into(output: &mut FunctionCallOutputPayload, later: Vec<FunctionCallOutputPayload>) {
    let own = parts(std::mem::replace(
        &mut output.body,
        FunctionCallOutputBody::Text(String::new()),
    ));
    let yielded = matches!(
        own.first(),
        Some(FunctionCallOutputContentItem::InputText { text }) if text.starts_with(YIELDED_PREFIX)
    );
    let later = later.into_iter().flat_map(|output| parts(output.body));
    let merged = if yielded {
        own.into_iter().chain(later).collect::<Vec<_>>()
    } else {
        later.chain(own).collect::<Vec<_>>()
    };
    output.body = FunctionCallOutputBody::ContentItems(merged);
}

#[cfg(test)]
#[path = "tool_output_merge_tests.rs"]
mod tests;
