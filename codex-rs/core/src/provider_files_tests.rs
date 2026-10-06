use super::*;
use codex_protocol::models::FunctionCallOutputPayload;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

const PNG: &str = "data:image/png;base64,AAEC";
const JPEG: &str = "data:image/jpeg;base64,AwQF";

fn user_image(url: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![
            ContentItem::InputText {
                text: "look".to_string(),
            },
            ContentItem::InputImage {
                image: ImageReference::Inline {
                    image_url: url.to_string(),
                },
                detail: None,
            },
        ],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn tool_images(urls: &[&str]) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: Some("call-1".to_string()),
        name: None,
        namespace: None,
        output: FunctionCallOutputPayload::from_content_items(
            urls.iter()
                .map(|url| FunctionCallOutputContentItem::InputImage {
                    image: ImageReference::Inline {
                        image_url: url.to_string(),
                    },
                    detail: None,
                })
                .collect(),
        ),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn file_ids(input: &[ResponseItem]) -> Vec<String> {
    serde_json::to_value(input)
        .unwrap()
        .to_string()
        .split("\"file_id\":\"")
        .skip(1)
        .map(|rest| rest.split('"').next().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn images_are_uploaded_once_and_referenced_by_file_id() {
    let provider = "https://files-once.test";
    let uploaded = Arc::new(AtomicUsize::new(0));
    let upload = |mime: String, bytes: Vec<u8>| {
        let uploaded = Arc::clone(&uploaded);
        async move {
            let n = uploaded.fetch_add(1, Ordering::SeqCst);
            Ok(format!("file-{n}-{mime}-{}", bytes.len()))
        }
    };
    let mut first = vec![user_image(PNG), tool_images(&[JPEG, PNG])];
    reference_uploaded_images(&mut first, provider, 86_400, upload).await;
    assert_eq!(uploaded.load(Ordering::SeqCst), 2);
    assert_eq!(
        file_ids(&first),
        ["file-0-image/png-3", "file-1-image/jpeg-3", "file-0-image/png-3"]
    );
    assert!(!serde_json::to_string(&first).unwrap().contains("base64"));

    // A later request with the same images: the same ids, nothing uploaded.
    let mut again = vec![tool_images(&[JPEG])];
    reference_uploaded_images(&mut again, provider, 86_400, upload).await;
    assert_eq!(uploaded.load(Ordering::SeqCst), 2);
    assert_eq!(file_ids(&again), ["file-1-image/jpeg-3"]);
}

#[tokio::test]
async fn a_failed_upload_leaves_the_image_inline_and_pauses_uploads() {
    let provider = "https://files-failing.test";
    let tried = Arc::new(AtomicUsize::new(0));
    let upload = |_: String, _: Vec<u8>| {
        let tried = Arc::clone(&tried);
        async move {
            tried.fetch_add(1, Ordering::SeqCst);
            Err::<String, _>(ApiError::Stream("down".to_string()))
        }
    };
    let mut input = vec![tool_images(&[PNG, JPEG])];
    let before = input.clone();
    reference_uploaded_images(&mut input, provider, 86_400, upload).await;
    assert_eq!(input, before);
    // Paused after the first failure: the second image was not tried.
    assert_eq!(tried.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_upload_near_its_expiry_is_replaced() {
    let provider = "https://files-expiring.test";
    let uploaded = Arc::new(AtomicUsize::new(0));
    let upload = |_: String, _: Vec<u8>| {
        let uploaded = Arc::clone(&uploaded);
        async move { Ok(format!("file-{}", uploaded.fetch_add(1, Ordering::SeqCst))) }
    };
    // Kept 0 s: renewed at once.
    let mut input = vec![user_image(PNG)];
    reference_uploaded_images(&mut input, provider, 0, upload).await;
    let mut again = vec![user_image(PNG)];
    reference_uploaded_images(&mut again, provider, 0, upload).await;
    assert_eq!(file_ids(&input), ["file-0"]);
    assert_eq!(file_ids(&again), ["file-1"]);
}
