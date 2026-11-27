use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn item(v: serde_json::Value) -> ResponseItem {
    serde_json::from_value(v).expect("valid response item")
}

fn user(text: &str) -> ResponseItem {
    item(
        json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}),
    )
}

fn assistant(text: &str) -> ResponseItem {
    item(
        json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}),
    )
}

fn developer(text: &str) -> ResponseItem {
    item(
        json!({"type": "message", "role": "developer", "content": [{"type": "input_text", "text": text}]}),
    )
}

fn call(id: &str) -> ResponseItem {
    item(
        json!({"type": "function_call", "name": "exec", "arguments": "{\"cmd\":\"ls\"}", "call_id": id}),
    )
}

fn output(id: &str, text: &str) -> ResponseItem {
    item(json!({"type": "function_call_output", "call_id": id, "output": text}))
}

fn envelopes(items: &[ResponseItem]) -> Vec<ResponseItemEnvelope> {
    items
        .iter()
        .cloned()
        .map(ResponseItemEnvelope::new)
        .collect()
}

#[test]
fn needs_parts_above_the_history_budget() {
    assert!(!needs_parts(150_000, 262_144));
    assert!(needs_parts(200_000, 262_144));
    assert!(needs_parts(330_549, 262_144));
}

#[test]
fn conversation_starts_after_the_instruction_prefix() {
    let items = [
        developer("rules"),
        developer("env"),
        user("hi"),
        assistant("hello"),
    ];
    assert_eq!(conversation_start(&envelopes(&items)), Some(2));
    let summary = part_summary_item("earlier");
    let items = [developer("rules"), summary, user("next")];
    assert_eq!(conversation_start(&envelopes(&items)), Some(1));
    assert_eq!(conversation_start(&envelopes(&[developer("rules")])), None);
}

fn open_calls(items: &[ResponseItem]) -> std::collections::HashSet<String> {
    let mut open = std::collections::HashSet::new();
    for (id, is_output) in items.iter().filter_map(call_link) {
        if is_output {
            open.remove(&id);
        } else {
            open.insert(id);
        }
    }
    open
}

#[test]
fn part_end_never_separates_a_call_from_its_output() {
    let big = "x".repeat(4_000); // ~1000 tokens
    let items = [
        user("q1"),
        call("c1"),
        output("c1", &big),
        call("c2"),
        call("c3"),
        output("c3", "done"),
        output("c2", &big),
        assistant("a1"),
        user("q2"),
        assistant("a2"),
    ];
    for budget in [1, 50, 500, 1_200, 2_500, 10_000, 1_000_000] {
        let end = part_end(&items, 0, budget).expect("a cut");
        assert!(end > 0 && end < items.len(), "budget {budget}: cut {end}");
        assert!(
            open_calls(&items[..end]).is_empty(),
            "budget {budget}: cut {end} leaves a call without its output"
        );
    }
    // Inside c2..c3 there is no valid cut, so a small budget never lands there.
    assert!(![4, 5, 6].contains(&part_end(&items, 3, 1).unwrap()));
}

#[test]
fn part_end_prefers_a_turn_boundary_and_keeps_the_last_item() {
    let items = [
        user("q1"),
        assistant("a1"),
        user("q2"),
        assistant("a2"),
        user("q3"),
    ];
    // Everything fits: the cut lands before the last user turn, never past it.
    let end = part_end(&items, 0, 1_000_000).unwrap();
    assert_eq!(end, 4);
    assert!(is_user_turn_boundary(&items[end]));
    // A single item cannot be split.
    assert_eq!(part_end(&[user("only")], 0, 10), None);
    // Nothing after the start but the last item.
    assert_eq!(part_end(&items, 4, 10), None);
}

#[test]
fn part_end_takes_one_oversized_item_rather_than_none() {
    let huge = "y".repeat(400_000);
    let items = [user(&huge), assistant("a"), user("q")];
    assert_eq!(part_end(&items, 0, 100), Some(1));
}

#[test]
fn render_part_keeps_everything_but_media() {
    let image = item(json!({"type": "message", "role": "user", "content": [
        {"type": "input_text", "text": "look"},
        {"type": "input_image", "image_url": "data:image/png;base64,AAAA"}
    ]}));
    let long = format!("{}needle-7731{}", "z".repeat(100_000), "z".repeat(100_000));
    let text = render_part(&[image, call("c1"), output("c1", &long)]);
    assert!(text.contains("[user]\nlook"));
    assert!(!text.contains("base64"));
    assert!(text.contains("[function_call]"));
    assert!(
        text.contains("needle-7731"),
        "the middle of a long output survives"
    );
}

#[test]
fn split_pieces_covers_the_text_within_the_limit() {
    let mut text = String::new();
    for i in 0..5_000 {
        text.push_str(&format!("第 {i} 行：一些内容 some content\n"));
    }
    text.push_str("暗号 copper-heron-5521\n");
    text.push_str(&"x".repeat(9_000)); // one line longer than a piece
    let pieces = split_pieces(&text, 4_096);
    assert!(pieces.len() > 10);
    assert!(pieces.iter().all(|p| p.len() <= 4_096));
    assert_eq!(pieces.concat(), text, "nothing dropped or reordered");
    assert!(pieces.iter().any(|p| p.contains("copper-heron-5521")));
    // Short text is one piece.
    assert_eq!(split_pieces("short", 4_096), vec!["short".to_owned()]);
}

#[test]
fn part_summaries_count_as_compaction_summaries() {
    let ResponseItem::Message { content, .. } = part_summary_item("s") else {
        panic!("a message");
    };
    let ContentItem::InputText { text } = &content[0] else {
        panic!("text");
    };
    assert!(is_summary_message(text));
    assert!(text.contains(PART_SUMMARY_HEADER));
}
