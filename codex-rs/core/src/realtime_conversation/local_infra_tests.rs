//! Tests of the local-infra conversation's own logic (no server, no thread).

use super::*;
use pretty_assertions::assert_eq;

struct Harness {
    conversation: LocalInfraConversation,
    out: tokio::sync::mpsc::UnboundedReceiver<Message>,
    _events: Receiver<RealtimeEvent>,
}

fn harness(group: bool) -> Harness {
    let (events_tx, events_rx) = async_channel::unbounded();
    let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel();
    Harness {
        conversation: LocalInfraConversation {
            sess: Weak::new(),
            sub_id: "sub".to_string(),
            group,
            idle_compact_percent: 0,
            events_tx,
            out: out_tx,
            resampler: Resampler::default(),
            output_rate: OUTPUT_RATE,
            started: true,
            speaking: false,
            listening: false,
            turn: Some("t1".to_string()),
            compacting: false,
            muted_turn: None,
            heard_since_cut: false,
            open_responses: HashMap::new(),
            items: HashMap::new(),
            context: Vec::new(),
            cut_off: None,
            last_answered: None,
            last_relays: Vec::new(),
            waiting: Vec::new(),
            host_context: None,
            waiting_since: None,
            relay_retry_at: None,
            idle_since: None,
            last_prompt: None,
            compact_floor: CompactFloor::None,
        },
        out: out_rx,
        _events: events_rx,
    }
}

impl Harness {
    /// The JSON events sent to the server so far.
    fn sent(&mut self) -> Vec<serde_json::Value> {
        std::iter::from_fn(|| self.out.try_recv().ok())
            .filter_map(|message| match message {
                Message::Text(text) => serde_json::from_str(&text).ok(),
                _ => None,
            })
            .collect()
    }

    async fn delta(&mut self, turn: &str, item: &str, delta: &str) {
        self.conversation
            .turn_signal(TurnSignal::Delta {
                turn_id: turn.to_string(),
                item_id: item.to_string(),
                delta: delta.to_string(),
            })
            .await;
    }

    async fn done(&mut self, turn: &str, item: &str, text: &str) {
        self.conversation
            .turn_signal(TurnSignal::MessageDone {
                turn_id: turn.to_string(),
                item_id: item.to_string(),
                text: text.to_string(),
            })
            .await;
    }
}

#[tokio::test]
async fn a_message_is_spoken_as_it_streams() {
    let mut h = harness(false);
    h.delta("t1", "m1", "好的，").await;
    h.delta("t1", "m1", "我查一下。").await;
    h.done("t1", "m1", "好的，我查一下。").await;
    assert_eq!(
        h.sent(),
        vec![
            json!({"type": "response.delta", "response_id": "m1", "text": "好的，"}),
            json!({"type": "response.delta", "response_id": "m1", "text": "我查一下。"}),
            json!({"type": "response.end", "response_id": "m1"}),
        ]
    );
}

#[tokio::test]
async fn words_are_spoken_whole() {
    let mut h = harness(false);
    h.delta("t1", "m1", "我在用 Git").await;
    h.delta("t1", "m1", "Hub 上找 Open").await;
    h.delta("t1", "m1", "AI 的 Codex, it wor").await;
    h.delta("t1", "m1", "ks well").await;
    h.done(
        "t1",
        "m1",
        "我在用 GitHub 上找 OpenAI 的 Codex, it works well",
    )
    .await;
    let texts: Vec<String> = h
        .sent()
        .iter()
        .filter_map(|m| m["text"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        texts,
        [
            "我在用 ",
            "GitHub 上找 ",
            "OpenAI 的 Codex, it ",
            "works ",
            "well"
        ]
    );
}

#[tokio::test]
async fn the_silence_marker_says_nothing() {
    let mut h = harness(false);
    h.delta("t1", "m1", "<sil").await;
    h.delta("t1", "m1", "ence>").await;
    h.delta("t1", "m1", " anything after it").await;
    h.done("t1", "m1", "<silence> anything after it").await;
    assert_eq!(h.sent(), Vec::<serde_json::Value>::new());
    assert!(h.conversation.open_responses.is_empty());
}

#[tokio::test]
async fn a_leading_aside_in_brackets_is_not_spoken() {
    let mut h = harness(false);
    h.delta("t1", "m1", "（他在跟").await;
    h.delta(
        "t1",
        "m1",
        "别人说话吗？）

到了",
    )
    .await;
    h.delta("t1", "m1", "，就在旁边（笑）。").await;
    h.done(
        "t1",
        "m1",
        "（他在跟别人说话吗？）

到了，就在旁边（笑）。",
    )
    .await;
    // A message all aside, and one not streamed.
    h.delta("t1", "m2", "(walked too far,").await;
    h.done("t1", "m2", "(walked too far, turning back)").await;
    h.done("t1", "m3", "（想想）(再想想) 好的").await;
    let texts: Vec<String> = h
        .sent()
        .iter()
        .filter_map(|m| m["text"].as_str().map(str::to_string))
        .collect();
    assert_eq!(texts.concat(), "到了，就在旁边（笑）。好的");
}

#[tokio::test]
async fn a_bare_tag_says_nothing() {
    let mut h = harness(false);
    h.delta("t1", "m1", "<").await;
    h.delta("t1", "m1", "s>").await;
    h.done("t1", "m1", "<s>").await;
    h.done(
        "t1", "m2", " </s>
",
    )
    .await;
    assert_eq!(h.sent(), Vec::<serde_json::Value>::new());
}

#[tokio::test]
async fn text_that_only_began_like_the_marker_is_spoken_whole() {
    let mut h = harness(false);
    h.delta("t1", "m1", "<").await;
    assert_eq!(h.sent(), Vec::<serde_json::Value>::new());
    h.delta("t1", "m1", "b>你好").await;
    h.done("t1", "m1", "<b>你好").await;
    assert_eq!(
        h.sent(),
        vec![
            json!({"type": "response.delta", "response_id": "m1", "text": "<b>你好"}),
            json!({"type": "response.end", "response_id": "m1"}),
        ]
    );
}

#[tokio::test]
async fn a_message_that_was_not_streamed_is_spoken_when_done() {
    let mut h = harness(false);
    h.done("t1", "m1", "三十七乘以四等于一百四十八。").await;
    assert_eq!(
        h.sent(),
        vec![
            json!({"type": "response.delta", "response_id": "m1", "text": "三十七乘以四等于一百四十八。"}),
            json!({"type": "response.end", "response_id": "m1"}),
        ]
    );
}

#[tokio::test]
async fn a_stopped_turn_says_nothing_more_and_ends_on_its_own() {
    let mut h = harness(false);
    h.conversation.turn = Some("t2".to_string());
    h.delta("t1", "old", "late words").await;
    h.done("t1", "old", "late words").await;
    h.conversation
        .turn_signal(TurnSignal::Finished {
            turn_id: Some("t1".to_string()),
            aborted: false,
        })
        .await;
    assert_eq!(h.sent(), Vec::<serde_json::Value>::new());
    assert_eq!(h.conversation.turn.as_deref(), Some("t2"));
    h.conversation
        .turn_signal(TurnSignal::Finished {
            turn_id: Some("t2".to_string()),
            aborted: false,
        })
        .await;
    assert_eq!(h.conversation.turn, None);
}

#[tokio::test]
async fn an_aborted_turn_ends_what_it_was_saying() {
    let mut h = harness(false);
    h.delta("t1", "m1", "从前有座山，").await;
    h.conversation
        .turn_signal(TurnSignal::Finished {
            turn_id: Some("t1".to_string()),
            aborted: false,
        })
        .await;
    assert_eq!(
        h.sent(),
        vec![
            json!({"type": "response.delta", "response_id": "m1", "text": "从前有座山，"}),
            json!({"type": "response.end", "response_id": "m1"}),
        ]
    );
}

#[tokio::test]
async fn the_next_input_carries_context_and_what_was_heard_of_a_cut_reply() {
    let mut h = harness(true);
    h.conversation
        .server_event(r#"{"type":"input.transcript","id":3,"text":"老王，吃饭去","respond":false}"#)
        .await;
    h.delta("t1", "m1", "从前有座山，山里有座庙。").await;
    h.conversation
        .server_event(
            r#"{"type":"response.done","response_id":"m1","spoken":"从前有座山，","cut":true}"#,
        )
        .await;
    assert_eq!(
        h.conversation.input_for("小乐，停一下"),
        "(Said meanwhile by others, not to you: 老王，吃饭去)\n\
         (Your last reply was cut off; the listener heard only: \"从前有座山，\")\n\
         小乐，停一下"
    );
    // Told once.
    assert_eq!(h.conversation.input_for("好的"), "好的");
}

#[tokio::test]
async fn one_to_one_talk_that_wants_no_reply_is_dropped() {
    let mut h = harness(false);
    h.conversation
        .server_event(r#"{"type":"input.transcript","id":1,"text":"嗯","respond":false}"#)
        .await;
    assert!(h.conversation.context.is_empty());
}

#[tokio::test]
async fn the_server_state_is_followed() {
    let mut h = harness(false);
    h.conversation.turn = None;
    assert!(h.conversation.idle());
    h.conversation
        .server_event(r#"{"type":"state","speaking":true,"listening":false}"#)
        .await;
    assert!(!h.conversation.idle());
    h.conversation
        .server_event(r#"{"type":"state","speaking":false,"listening":false}"#)
        .await;
    assert!(h.conversation.idle());
}

fn frame(samples: &[i16], rate: u32, channels: u16) -> RealtimeAudioFrame {
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    RealtimeAudioFrame {
        data: BASE64_STANDARD.encode(bytes),
        sample_rate: rate,
        num_channels: channels,
        samples_per_channel: None,
        item_id: None,
    }
}

#[test]
fn audio_at_the_server_rate_passes_through() {
    let mut resampler = Resampler::default();
    let samples: Vec<i16> = (0..320).map(|i| i as i16).collect();
    let out = resampler.input_pcm(&frame(&samples, 16_000, 1)).unwrap();
    assert_eq!(out.len(), 640);
    assert_eq!(i16::from_le_bytes([out[2], out[3]]), 1);
}

#[test]
fn stereo_48k_becomes_16k_mono_without_losing_samples_across_frames() {
    let mut resampler = Resampler::default();
    let mut total = 0;
    for _ in 0..50 {
        // 20 ms of stereo 48 kHz, both channels at 300.
        let samples = vec![300_i16; 960 * 2];
        let out = resampler.input_pcm(&frame(&samples, 48_000, 2)).unwrap();
        assert!(
            out.chunks_exact(2)
                .all(|b| i16::from_le_bytes([b[0], b[1]]) == 300)
        );
        total += out.len() / 2;
    }
    // One second of audio: 16000 samples, give or take the last one.
    assert!((15_999..=16_000).contains(&total), "{total}");
}

#[test]
fn audio_at_24k_is_resampled() {
    let mut resampler = Resampler::default();
    let mut total = 0;
    for _ in 0..50 {
        let out = resampler
            .input_pcm(&frame(&vec![0_i16; 480], 24_000, 1))
            .unwrap();
        total += out.len() / 2;
    }
    assert!((15_999..=16_000).contains(&total), "{total}");
}

#[tokio::test]
async fn a_cut_turn_says_nothing_more_and_leaves_nothing_open() {
    let mut h = harness(false);
    h.delta("t1", "m1", "从前有座山，").await;
    h.sent();
    h.conversation
        .server_event(
            r#"{"type":"response.done","response_id":"m1","spoken":"从前有座山，","cut":true}"#,
        )
        .await;
    // The turn goes on generating: none of it is spoken, a new message
    // included, and nothing stays open.
    h.delta("t1", "m1", "山里有座庙。").await;
    h.delta("t1", "m2", "我接着讲。").await;
    h.done("t1", "m2", "我接着讲。").await;
    assert_eq!(h.sent(), Vec::<serde_json::Value>::new());
    assert!(h.conversation.open_responses.is_empty());
    h.conversation
        .turn_signal(TurnSignal::Finished {
            turn_id: Some("t1".to_string()),
            aborted: false,
        })
        .await;
    assert!(h.conversation.idle());
    assert_eq!(h.conversation.cut_off.as_deref(), Some("从前有座山，"));
}

#[tokio::test]
async fn what_was_heard_of_several_cut_replies_adds_up() {
    let mut h = harness(false);
    h.delta("t1", "m1", "第一句。").await;
    h.delta("t1", "m2", "第二句。").await;
    for (id, spoken) in [("m1", "第一句。"), ("m2", "")] {
        h.conversation
            .server_event(&format!(
                r#"{{"type":"response.done","response_id":"{id}","spoken":"{spoken}","cut":true}}"#
            ))
            .await;
    }
    assert_eq!(h.conversation.cut_off.as_deref(), Some("第一句。"));
}

#[tokio::test]
async fn a_cut_of_a_replaced_turn_is_not_told_to_the_next_one() {
    let mut h = harness(false);
    h.delta("t1", "m1", "旧的回答。").await;
    // Already answering a newer utterance.
    h.conversation.turn = Some("t2".to_string());
    h.conversation
        .server_event(r#"{"type":"response.done","response_id":"m1","spoken":"旧的","cut":true}"#)
        .await;
    assert_eq!(h.conversation.cut_off, None);
}

#[tokio::test]
async fn an_utterance_that_goes_on_replaces_its_context() {
    let mut h = harness(true);
    h.conversation
        .server_event(r#"{"type":"input.transcript","id":3,"text":"老王","respond":false}"#)
        .await;
    h.conversation
        .server_event(
            r#"{"type":"input.transcript","id":4,"text":"老王，吃饭去","replaces":3,"respond":false}"#,
        )
        .await;
    assert_eq!(
        h.conversation.context,
        vec![(4, "老王，吃饭去".to_string())]
    );
}

#[tokio::test]
async fn after_a_compaction_the_prompt_must_grow_again() {
    let mut h = harness(false);
    h.conversation.compact_floor = CompactFloor::AfterNextPrompt;
    h.conversation
        .turn_signal(TurnSignal::Tokens {
            input: 60_000,
            window: Some(100_000),
        })
        .await;
    assert_eq!(h.conversation.compact_floor, CompactFloor::Tokens(70_000));
}

#[tokio::test]
async fn the_marker_cut_short_at_the_end_is_not_spoken() {
    let mut h = harness(false);
    h.delta("t1", "m1", "<sil").await;
    h.done("t1", "m1", "<sil").await;
    h.done("t1", "m2", "<silence>\n(nothing to say)").await;
    assert_eq!(h.sent(), Vec::<serde_json::Value>::new());
}

#[tokio::test]
async fn words_still_waiting_go_with_the_next_utterance() {
    let mut h = harness(false);
    h.conversation
        .wait_to_tell("(The backend finished \"weather\": sunny)".to_string());
    assert_eq!(
        h.conversation.input_for("小乐，还有呢？"),
        "(The backend finished \"weather\": sunny)\n小乐，还有呢？"
    );
    assert!(h.conversation.waiting.is_empty());
    assert_eq!(h.conversation.waiting_since, None);
}

#[tokio::test]
async fn a_cut_by_talk_that_was_no_utterance_lets_the_turn_speak_again() {
    let mut h = harness(false);
    h.delta("t1", "m1", "第一段。").await;
    h.conversation
        .server_event(r#"{"type":"state","speaking":true,"listening":true}"#)
        .await;
    h.conversation
        .server_event(r#"{"type":"response.done","response_id":"m1","spoken":"","cut":true}"#)
        .await;
    h.sent();
    // A cough: listening ends with no transcript.
    h.conversation
        .server_event(r#"{"type":"state","speaking":false,"listening":false}"#)
        .await;
    // The cut message is over for the server: only the next one is spoken.
    h.delta("t1", "m1", "第二段。").await;
    h.done("t1", "m1", "第一段。第二段。").await;
    h.delta("t1", "m2", "结果是晴天。").await;
    assert_eq!(
        h.sent(),
        vec![json!({"type": "response.delta", "response_id": "m2", "text": "结果是晴天。"})]
    );
    assert_eq!(
        h.conversation.open_responses,
        HashMap::from([("m2".to_string(), "t1".to_string())])
    );
}

#[tokio::test]
async fn a_compaction_counts_nothing_and_its_end_sets_the_floor() {
    let mut h = harness(false);
    h.conversation.compacting = true;
    h.conversation
        .turn_signal(TurnSignal::Tokens {
            input: 90_000,
            window: Some(100_000),
        })
        .await;
    assert_eq!(h.conversation.last_prompt, None);
    h.conversation
        .turn_signal(TurnSignal::Finished {
            turn_id: Some("t1".to_string()),
            aborted: false,
        })
        .await;
    assert_eq!(h.conversation.compact_floor, CompactFloor::AfterNextPrompt);
    // A compaction stopped by an utterance is simply tried again.
    h.conversation.turn = Some("t2".to_string());
    h.conversation.compacting = true;
    h.conversation
        .turn_signal(TurnSignal::Finished {
            turn_id: Some("t2".to_string()),
            aborted: true,
        })
        .await;
    assert_eq!(h.conversation.compact_floor, CompactFloor::None);
}

#[tokio::test]
async fn words_a_turn_was_started_with_are_kept_until_it_ends() {
    let mut h = harness(false);
    h.conversation.last_relays = vec!["(The backend finished \"weather\": sunny)".to_string()];
    // Stopped: told again with the next answer.
    h.conversation
        .turn_signal(TurnSignal::Finished {
            turn_id: Some("t1".to_string()),
            aborted: true,
        })
        .await;
    assert_eq!(h.conversation.last_relays.len(), 1);
    h.conversation.turn = Some("t2".to_string());
    h.conversation
        .turn_signal(TurnSignal::Finished {
            turn_id: Some("t2".to_string()),
            aborted: false,
        })
        .await;
    assert_eq!(h.conversation.last_relays, Vec::<String>::new());
}

#[tokio::test]
async fn the_hosts_latest_context_goes_with_the_next_input_once() {
    let mut h = harness(false);
    h.conversation.host_context = Some("(Room: Home; here: Alice)".to_string());
    // A newer one replaces it.
    h.conversation.host_context = Some("(Room: Garden; here: Alice, Bob)".to_string());
    assert_eq!(
        h.conversation.input_for("Jarvis, hi"),
        "(Room: Garden; here: Alice, Bob)\nJarvis, hi"
    );
    assert_eq!(h.conversation.input_for("Jarvis, again"), "Jarvis, again");
}

#[tokio::test]
async fn a_streamed_bare_tag_then_a_newline_says_nothing() {
    let mut h = harness(false);
    h.delta("t1", "m1", "<s>").await;
    h.delta("t1", "m1", "\n").await;
    h.done("t1", "m1", "<s>\n").await;
    h.delta("t1", "m2", "</s>\n").await;
    h.done("t1", "m2", "</s>\n").await;
    assert_eq!(h.sent(), Vec::<serde_json::Value>::new());
}

#[tokio::test]
async fn a_tag_like_last_word_of_speech_is_spoken() {
    let mut h = harness(false);
    h.delta("t1", "m1", "Press <Enter>").await;
    h.done("t1", "m1", "Press <Enter>").await;
    let texts: Vec<String> = h
        .sent()
        .iter()
        .filter_map(|m| m["text"].as_str().map(str::to_string))
        .collect();
    assert_eq!(texts.concat(), "Press <Enter>");
}
