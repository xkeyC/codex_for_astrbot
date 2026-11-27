//! Fork addition: `inspect(...values)` shows strings, images and JSON.

use std::sync::Arc;

use codex_code_mode_runtime::ExecuteRequest;
use codex_code_mode_runtime::FunctionCallOutputContentItem;
use codex_code_mode_runtime::InProcessCodeModeSession;
use codex_code_mode_runtime::NoopCodeModeSessionDelegate;
use codex_code_mode_runtime::RuntimeResponse;
use pretty_assertions::assert_eq;

const PIXEL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

#[allow(clippy::expect_used)]
async fn run(source: &str) -> Vec<FunctionCallOutputContentItem> {
    let session = InProcessCodeModeSession::new();
    let started = session
        .execute(
            ExecuteRequest {
                language: Default::default(),
                tool_call_id: "inspect".to_string(),
                enabled_tools: Vec::new(),
                source: source.to_string(),
                yield_time_ms: Some(/*value*/ 5_000),
                max_output_tokens: None,
            },
            Arc::new(NoopCodeModeSessionDelegate),
            /*preempt*/ None,
        )
        .await
        .expect("start cell");
    let response = started.initial_response().await.expect("execute cell");
    let RuntimeResponse::Result {
        content_items,
        error_text: None,
        ..
    } = response
    else {
        panic!("unexpected cell response: {response:?}");
    };
    content_items
}

fn text(text: &str) -> FunctionCallOutputContentItem {
    FunctionCallOutputContentItem::InputText {
        text: text.to_string(),
    }
}

#[tokio::test]
async fn inspect_shows_strings_images_and_json() {
    let cyclic = "const cyclic = {}; cyclic.self = cyclic;";
    let items = run(&format!(
        "{cyclic}\ninspect(\"plain\", {{a: [1, 2]}}, \"{PIXEL}\", {{image_url: \"{PIXEL}\"}}, undefined, 7n, cyclic, {{image_url: \"not an image\"}});"
    ))
    .await;
    assert_eq!(items.len(), 8, "{items:?}");
    assert_eq!(
        items[..2],
        [text("plain"), text("{\n  \"a\": [\n    1,\n    2\n  ]\n}")]
    );
    for image in &items[2..4] {
        assert!(
            matches!(image, FunctionCallOutputContentItem::InputImage { image_url, .. } if image_url == PIXEL),
            "{image:?}"
        );
    }
    assert_eq!(items[4..6], [text("undefined"), text("7n")]);
    assert_eq!(items[6], text("[object Object]"));
    // A value the image helper refuses is shown as JSON instead.
    assert_eq!(items[7], text("{\n  \"image_url\": \"not an image\"\n}"));
}
