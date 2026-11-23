use super::*;
use pretty_assertions::assert_eq;

fn function_output(call_id: &str, body: FunctionCallOutputBody) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: Some(call_id.to_string()),
        name: None,
        namespace: None,
        output: FunctionCallOutputPayload {
            body,
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    }
}

fn custom_output(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: call_id.to_string(),
        name: None,
        output: FunctionCallOutputPayload::from_text(text.to_string()),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn text(text: &str) -> FunctionCallOutputContentItem {
    FunctionCallOutputContentItem::InputText {
        text: text.to_string(),
    }
}

fn texts(parts: &[&str]) -> FunctionCallOutputBody {
    FunctionCallOutputBody::ContentItems(parts.iter().map(|part| text(part)).collect())
}

#[test]
fn notifications_read_before_a_final_result() {
    let mut input = vec![
        function_output("call-1", texts(&["Script completed\nOutput:\n", "done"])),
        function_output("call-2", texts(&["other"])),
        function_output(
            "call-1",
            FunctionCallOutputBody::Text("working…".to_string()),
        ),
    ];
    merge_duplicate_tool_outputs(&mut input, /*custom*/ false);
    assert_eq!(
        input,
        vec![
            function_output(
                "call-1",
                texts(&["working…", "Script completed\nOutput:\n", "done"])
            ),
            function_output("call-2", texts(&["other"])),
        ]
    );
}

#[test]
fn notifications_read_after_a_yield() {
    let image = FunctionCallOutputContentItem::InputImage {
        image: codex_protocol::models::ImageReference::Inline {
            image_url: "data:image/png;base64,AAAA".to_string(),
        },
        detail: None,
    };
    let mut input = vec![
        function_output(
            "call-1",
            texts(&["Script running with cell ID 7\nWall time 1.0 seconds\nOutput:\n"]),
        ),
        function_output("call-1", FunctionCallOutputBody::Text("step 1".to_string())),
        function_output(
            "call-1",
            FunctionCallOutputBody::ContentItems(vec![image.clone()]),
        ),
    ];
    merge_duplicate_tool_outputs(&mut input, /*custom*/ false);
    assert_eq!(
        input,
        vec![function_output(
            "call-1",
            FunctionCallOutputBody::ContentItems(vec![
                text("Script running with cell ID 7\nWall time 1.0 seconds\nOutput:\n"),
                text("step 1"),
                image,
            ]),
        )]
    );
}

#[test]
fn custom_outputs_merge_only_when_asked() {
    let input = vec![
        custom_output("exec-1", "done"),
        custom_output("exec-1", "note"),
    ];
    let mut kept = input.clone();
    merge_duplicate_tool_outputs(&mut kept, /*custom*/ false);
    assert_eq!(kept, input);
    let mut merged = input;
    merge_duplicate_tool_outputs(&mut merged, /*custom*/ true);
    assert_eq!(merged.len(), 1);
    let ResponseItem::CustomToolCallOutput { output, .. } = &merged[0] else {
        panic!("a custom output");
    };
    assert_eq!(output.body, texts(&["note", "done"]));
}
