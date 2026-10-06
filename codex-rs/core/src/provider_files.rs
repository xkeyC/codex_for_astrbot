//! AstrBot: a request's inline images go to the provider's Files API once and
//! are referenced by file id (`model_provider_options.<id>.files_api`).
//!
//! Codex sends the whole history with every request, so an image a tool
//! returned would go out again with each one (a voice thread walking by
//! pictures carries megabytes). Uploaded, it is sent once; its file id is
//! kept per image (by content) for the process, so every later request
//! refers to it the same way and the provider's prefix cache still matches.
//! History itself keeps the inline image: a provider without the option, or
//! an upload gone, gets it as before.

use std::collections::HashMap;
use std::future::Future;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use codex_api::ApiError;
use codex_api::decode_image_data_url;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ImageReference;
use codex_protocol::models::ResponseItem;
use sha1::Digest;
use sha1::Sha1;
use tracing::info;
use tracing::warn;

/// An upload is replaced this long before the provider deletes it.
const RENEW_BEFORE: Duration = Duration::from_secs(3600);
/// After a failed upload, a provider's images go inline for this long.
const PAUSE_AFTER_FAILURE: Duration = Duration::from_secs(60);

struct Upload {
    file_id: String,
    renew_at: Instant,
}

#[derive(Default)]
struct Uploads {
    /// By provider (its base URL) and image (SHA-1 of its data URL).
    files: HashMap<(String, [u8; 20]), Upload>,
    paused_until: HashMap<String, Instant>,
}

static UPLOADS: LazyLock<Mutex<Uploads>> = LazyLock::new(Mutex::default);

fn uploads() -> std::sync::MutexGuard<'static, Uploads> {
    UPLOADS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The inline image data URLs of `input`, each once, in order.
fn inline_images(input: &[ResponseItem]) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let mut add = |url: &String| {
        if url.starts_with("data:") && !urls.contains(url) {
            urls.push(url.clone());
        }
    };
    for item in input {
        match item {
            ResponseItem::Message { content, .. } => {
                for part in content {
                    if let ContentItem::InputImage {
                        image: ImageReference::Inline { image_url },
                        ..
                    } = part
                    {
                        add(image_url);
                    }
                }
            }
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => {
                if let FunctionCallOutputBody::ContentItems(items) = &output.body {
                    for part in items {
                        if let FunctionCallOutputContentItem::InputImage {
                            image: ImageReference::Inline { image_url },
                            ..
                        } = part
                        {
                            add(image_url);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    urls
}

/// Replaces the inline images of `input` that have a file id.
fn reference(input: &mut [ResponseItem], ids: &HashMap<String, String>) {
    let swap = |image: &mut ImageReference| {
        if let ImageReference::Inline { image_url } = image
            && let Some(file_id) = ids.get(image_url.as_str())
        {
            *image = ImageReference::File {
                file_id: file_id.clone(),
            };
        }
    };
    for item in input {
        match item {
            ResponseItem::Message { content, .. } => {
                for part in content {
                    if let ContentItem::InputImage { image, .. } = part {
                        swap(image);
                    }
                }
            }
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => {
                if let FunctionCallOutputBody::ContentItems(items) = &mut output.body {
                    for part in items {
                        if let FunctionCallOutputContentItem::InputImage { image, .. } = part {
                            swap(image);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Refers to `input`'s inline images by the file ids of their uploads to
/// `provider` (its base URL), uploading those not uploaded yet with
/// `upload(mime, bytes)`. An image that cannot be uploaded stays inline.
pub(crate) async fn reference_uploaded_images<F, Fut>(
    input: &mut [ResponseItem],
    provider: &str,
    expires_seconds: u64,
    upload: F,
) where
    F: Fn(String, Vec<u8>) -> Fut,
    Fut: Future<Output = Result<String, ApiError>>,
{
    let urls = inline_images(input);
    if urls.is_empty() {
        return;
    }
    let lifetime = Duration::from_secs(expires_seconds);
    let mut ids = HashMap::new();
    for url in urls {
        let key = (provider.to_string(), Sha1::digest(url.as_bytes()).into());
        {
            let now = Instant::now();
            let uploads = uploads();
            if let Some(found) = uploads.files.get(&key)
                && now < found.renew_at
            {
                ids.insert(url, found.file_id.clone());
                continue;
            }
            if uploads
                .paused_until
                .get(provider)
                .is_some_and(|until| now < *until)
            {
                continue;
            }
        }
        let Some((mime, bytes)) = decode_image_data_url(&url) else {
            continue;
        };
        let size = bytes.len();
        let started = Instant::now();
        match upload(mime, bytes).await {
            Ok(file_id) => {
                info!(
                    file_id,
                    bytes = size,
                    upload_ms = started.elapsed().as_millis() as u64,
                    "image uploaded to the provider's Files API"
                );
                let mut uploads = uploads();
                let now = Instant::now();
                uploads.files.retain(|_, upload| now < upload.renew_at);
                uploads.files.insert(
                    key,
                    Upload {
                        file_id: file_id.clone(),
                        renew_at: started + lifetime.saturating_sub(RENEW_BEFORE.min(lifetime / 2)),
                    },
                );
                ids.insert(url, file_id);
            }
            Err(err) => {
                warn!("image upload to the provider's Files API failed, sent inline: {err}");
                uploads()
                    .paused_until
                    .insert(provider.to_string(), Instant::now() + PAUSE_AFTER_FAILURE);
            }
        }
    }
    reference(input, &ids);
}

#[cfg(test)]
#[path = "provider_files_tests.rs"]
mod tests;
