//! AstrBot: compaction of a history larger than the compacting model's window.
//!
//! Local compaction sends the whole history plus the summarization prompt in
//! one request. When the history outgrew the model (a long thread moved to a
//! model with a smaller window, or one huge turn), that request cannot fit,
//! and the old fallback dropped the oldest items unread until it did.
//!
//! Instead, the oldest part is summarized on its own first: a prefix of whole
//! items (never splitting a tool call from its output) is rendered as a plain
//! transcript, read in pieces that each fit one request (a rolling summary
//! carried from piece to piece, so one huge message or tool output is read in
//! full too), and replaced by the resulting summary.
//! This repeats, folding each earlier summary into the next part, until the
//! rest fits; the regular compaction then summarizes the result. The partial
//! summaries live only in the compaction prompt, never in the session.
//! OpenAI providers keep upstream's behaviour.

use std::collections::HashSet;
use std::sync::Arc;

use codex_history::ResponseItemEnvelope;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_utils_output_truncation::approx_token_count;
use serde_json::Value;
use tracing::info;
use tracing::warn;

use crate::Prompt;
use crate::client::ModelClientSession;
use crate::compact::SUMMARY_PREFIX;
use crate::compact::content_items_to_text;
use crate::compact::drain_summary_only;
use crate::compact::is_summary_message;
use crate::context_manager::ContextManager;
use crate::context_manager::estimate_item_token_count;
use crate::context_manager::is_user_turn_boundary;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn::get_last_assistant_message_from_turn;

/// The compaction request (history + prompt + summary) should stay under this
/// share of the window; token estimates are approximate.
const HISTORY_BUDGET_PERCENT: i64 = 70;
/// One part's transcript, as a share of the window, before shrinking.
const PART_BUDGET_PERCENT: i64 = 45;
/// Shrinking stops here: a part this small that still overflows is an error.
const MIN_PART_BUDGET_PERCENT: i64 = 5;
const MAX_ROUNDS: usize = 24;
/// Requests for one part, at most (a part's transcript is cut into pieces).
const MAX_PIECES: usize = 64;
/// Bytes per token, as approx_token_count counts them.
const BYTES_PER_TOKEN: usize = 4;
/// One item's text is cut beyond this (a multi-megabyte tool output would
/// otherwise cost dozens of requests for little).
const MAX_ITEM_BYTES: usize = 2_000_000;

pub(crate) const PART_SUMMARY_HEADER: &str = "Summary of an earlier part of this conversation (written while compacting a history too long for one request):";

const PART_PROMPT: &str = "You are compacting a long conversation that does not fit in one request. \
You read its OLDEST part as a transcript (oldest first), one fragment at a time; the transcript may begin with a summary of what came before it. \
If a <summary_so_far> is given, it covers the earlier fragments: return it updated with the new fragment, as one summary. \
The summary replaces everything it covers, so keep what a later reader needs: the user's requests and preferences, decisions and their reasons, \
facts and results established (file paths, identifiers, codes, numbers, commands, URLs), tool results that matter, and anything left unfinished. \
Be complete but concise; reply with the summary only.";

/// Whether the oldest part of `history` has to be summarized before the
/// regular compaction request can fit a `window`-token model.
pub(crate) fn needs_parts(history_tokens: i64, window: i64) -> bool {
    history_tokens > window.saturating_mul(HISTORY_BUDGET_PERCENT) / 100
}

fn tokens(item: &ResponseItem) -> i64 {
    estimate_item_token_count(item).max(1)
}

fn is_summary_item(item: &ResponseItem) -> bool {
    let ResponseItem::Message { role, content, .. } = item else {
        return false;
    };
    role == "user"
        && content
            .iter()
            .any(|part| matches!(part, ContentItem::InputText { text } if is_summary_message(text)))
}

/// Where the conversation proper starts: after the leading instruction and
/// context items, which the summary request must not swallow.
pub(crate) fn conversation_start(items: &[ResponseItemEnvelope]) -> Option<usize> {
    items
        .iter()
        .position(|e| is_user_turn_boundary(&e.item) || is_summary_item(&e.item))
}

/// A call or output's (call id, is_output), from its wire form.
fn call_link(item: &ResponseItem) -> Option<(String, bool)> {
    let v = serde_json::to_value(item).ok()?;
    let kind = v.get("type")?.as_str()?;
    let id = v.get("call_id")?.as_str()?.to_owned();
    match kind {
        "function_call" | "custom_tool_call" | "tool_search_call" | "local_shell_call" => {
            Some((id, false))
        }
        "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
            Some((id, true))
        }
        _ => None,
    }
}

/// The end (exclusive) of the oldest part starting at `start`: the largest cut
/// within `budget` tokens that leaves no tool call without its output on the
/// summarized side, preferring cuts before a user turn. A first item over the
/// budget makes a part of one item (its transcript is shortened). The last
/// item always stays.
pub(crate) fn part_end(items: &[ResponseItem], start: usize, budget: i64) -> Option<usize> {
    if items.len() < 2 || start >= items.len() - 1 {
        return None;
    }
    let mut open: HashSet<String> = HashSet::new();
    let mut used = 0i64;
    let mut best_any = None;
    let mut best_turn = None;
    let mut first = None;
    for (i, item) in items.iter().enumerate().take(items.len() - 1).skip(start) {
        used = used.saturating_add(tokens(item));
        match call_link(item) {
            Some((id, false)) => {
                open.insert(id);
            }
            Some((id, true)) => {
                open.remove(&id);
            }
            None => {}
        }
        let cut = i + 1;
        if !open.is_empty() {
            continue;
        }
        if first.is_none() {
            first = Some(cut);
        }
        if used > budget {
            break;
        }
        best_any = Some(cut);
        if is_user_turn_boundary(&items[cut]) {
            best_turn = Some(cut);
        }
    }
    // A turn boundary is a cleaner cut unless it leaves most of the budget unused.
    match (best_turn, best_any) {
        (Some(t), Some(a)) => {
            let turn_tokens: i64 = items[start..t].iter().map(tokens).sum();
            Some(if turn_tokens * 2 >= budget { t } else { a })
        }
        (None, Some(a)) => Some(a),
        _ => first,
    }
}

/// Cuts `s` to at most `max_bytes`, keeping its start and end, on char boundaries.
fn clip(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let keep = max_bytes.saturating_sub(64);
    let mut head = keep * 3 / 4;
    while !s.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = s.len() - keep / 4;
    while !s.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n[… {} bytes omitted …]\n{}",
        &s[..head],
        tail - head,
        &s[tail..]
    )
}

/// Drops what a summarizer cannot use (encrypted blobs, ids, inline media).
fn sanitize(v: &mut Value) {
    match v {
        Value::String(s) => {
            if s.starts_with("data:") {
                *s = "[inline data omitted]".to_owned();
            }
        }
        Value::Array(a) => a.iter_mut().for_each(sanitize),
        Value::Object(o) => {
            for k in ["encrypted_content", "id", "status"] {
                o.remove(k);
            }
            o.values_mut().for_each(sanitize);
        }
        _ => {}
    }
}

fn render_item(item: &ResponseItem) -> Option<String> {
    let text = match item {
        ResponseItem::Reasoning { .. } => return None,
        ResponseItem::Message { role, content, .. } => {
            format!("[{role}]\n{}", content_items_to_text(content)?)
        }
        _ => {
            let mut v = serde_json::to_value(item).ok()?;
            let kind = v
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("item")
                .to_owned();
            sanitize(&mut v);
            if let Value::Object(o) = &mut v {
                o.remove("type");
            }
            format!("[{kind}]\n{}", serde_json::to_string(&v).ok()?)
        }
    };
    Some(clip(&text, MAX_ITEM_BYTES))
}

/// A part as one plain transcript, in full (inline media and encrypted blobs
/// left out).
pub(crate) fn render_part(items: &[ResponseItem]) -> String {
    items
        .iter()
        .filter_map(render_item)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Cuts `text` into pieces of at most `max_bytes`, at line breaks where it
/// can; nothing is dropped.
pub(crate) fn split_pieces(text: &str, max_bytes: usize) -> Vec<String> {
    let max_bytes = max_bytes.max(1024);
    let mut pieces = Vec::new();
    let mut rest = text;
    while rest.len() > max_bytes {
        let mut cut = max_bytes;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        // Prefer the last line break in the second half of the window.
        if let Some(nl) = rest[..cut].rfind('\n')
            && nl >= cut / 2
        {
            cut = nl + 1;
        }
        pieces.push(rest[..cut].to_owned());
        rest = &rest[cut..];
    }
    if !rest.is_empty() || pieces.is_empty() {
        pieces.push(rest.to_owned());
    }
    pieces
}

fn user_text(text: String) -> ResponseItem {
    ResponseItem::Message {
        status: None,
        encrypted_content: None,
        id: None,
        role: "user".to_owned(),
        content: vec![ContentItem::InputText { text }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

pub(crate) fn part_summary_item(summary: &str) -> ResponseItem {
    user_text(format!(
        "{SUMMARY_PREFIX}\n{PART_SUMMARY_HEADER}\n{summary}"
    ))
}

/// Summarizes the oldest parts of `history` until it fits the compaction
/// request of `step_context`'s model. A no-op for a history that fits or a
/// model without a known window.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn summarize_oversized_history(
    request: &mut crate::hook_runtime::sampling::SamplingRequest,
    sess: &Arc<Session>,
    step_context: &Arc<StepContext>,
    client_session: &mut ModelClientSession,
    responses_metadata: &CodexResponsesMetadata,
    history: &mut ContextManager,
    base_instructions: &BaseInstructions,
) -> CodexResult<()> {
    // OpenAI keeps upstream's behaviour (and has remote compaction); the
    // providers AstrBot adds (open models, DeepSeek, gateways) get parts.
    if step_context.turn.provider.info().is_openai() {
        return Ok(());
    }
    let Some(window) = step_context.turn.model_context_window().filter(|w| *w > 0) else {
        return Ok(());
    };
    let mut part_percent = PART_BUDGET_PERCENT;
    for round in 0..MAX_ROUNDS {
        let estimate = history
            .estimate_token_count_with_base_instructions(base_instructions)
            .unwrap_or(0);
        if !needs_parts(estimate, window) {
            if round > 0 {
                info!(
                    estimate,
                    window,
                    rounds = round,
                    "oversized history summarized in parts"
                );
            }
            return Ok(());
        }
        let envelopes = history.annotated_items().to_vec();
        let Some(start) = conversation_start(&envelopes) else {
            return Ok(());
        };
        let items: Vec<ResponseItem> = envelopes.iter().map(|e| e.item.clone()).collect();
        let budget = window.saturating_mul(part_percent) / 100;
        let Some(end) = part_end(&items, start, budget) else {
            return Ok(()); // nothing left to fold; the regular fallback takes over
        };
        let transcript = render_part(&items[start..end]);
        let budget_bytes = usize::try_from(budget)
            .unwrap_or(usize::MAX)
            .saturating_mul(BYTES_PER_TOKEN);
        let pieces = split_pieces(&transcript, budget_bytes);
        if pieces.len() > MAX_PIECES {
            warn!(
                pieces = pieces.len(),
                "an oversized part needs too many requests"
            );
            return Ok(()); // the regular fallback takes over
        }
        info!(
            round,
            estimate,
            window,
            start,
            end,
            part_tokens = approx_token_count(&transcript),
            pieces = pieces.len(),
            "summarizing the oldest part of an oversized history"
        );
        // A rolling summary over the part's pieces.
        let mut summary: Option<String> = None;
        let mut overflowed = None;
        for (i, piece) in pieces.iter().enumerate() {
            let carried = summary
                .as_deref()
                .map(|s| format!("<summary_so_far>\n{s}\n</summary_so_far>\n\n"))
                .unwrap_or_default();
            let text = format!(
                "{PART_PROMPT}\n\n{carried}<transcript fragment=\"{} of {}\">\n{piece}\n</transcript>",
                i + 1,
                pieces.len()
            );
            let prompt = Prompt {
                input: vec![user_text(text)],
                base_instructions: base_instructions.clone(),
                prompt_cache_ttl: step_context.turn.prompt_cache_ttl,
                cyber_access_program: step_context.turn.cyber_access_program,
                ..Default::default()
            };
            match drain_summary_only(
                request,
                sess,
                step_context,
                client_session,
                responses_metadata,
                &Arc::new(prompt),
            )
            .await
            {
                Ok(output) => {
                    if let Some(s) = get_last_assistant_message_from_turn(output.iter())
                        .filter(|s| !s.trim().is_empty())
                    {
                        summary = Some(s);
                    }
                }
                Err(e) if matches!(e.details(), CodexErrorDetails::ContextWindowExceeded) => {
                    overflowed = Some(e);
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        if let Some(e) = overflowed {
            part_percent /= 2;
            warn!(
                part_percent,
                "a history piece overflowed the window; halving the pieces"
            );
            if part_percent < MIN_PART_BUDGET_PERCENT {
                return Err(e);
            }
            continue;
        }
        let summary =
            summary.unwrap_or_else(|| "(the summarizer returned nothing for this part)".to_owned());
        let mut next: Vec<ResponseItemEnvelope> = envelopes[..start].to_vec();
        next.push(ResponseItemEnvelope::new(part_summary_item(&summary)));
        next.extend_from_slice(&envelopes[end..]);
        crate::context_manager::remove_orphan_outputs(&mut next);
        history.replace_annotated(next);
    }
    Ok(())
}

#[cfg(test)]
#[path = "compact_chunked_tests.rs"]
mod tests;
