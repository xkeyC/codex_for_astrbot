//! AstrBot: a provider's Files API (`POST /files`, OpenAI's shape, which
//! DeepSeek follows): an image uploaded once is then referenced by its file
//! id instead of carried inline in every request.

use crate::auth::SharedAuthProvider;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use base64::Engine;
use bytes::Bytes;
use codex_client::HttpTransport;
use codex_client::RequestBody;
use codex_client::RequestTelemetry;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::header::CONTENT_TYPE;
use serde::Deserialize;
use std::sync::Arc;

const MULTIPART_BOUNDARY: &str = "codex-files-boundary-7f3a9c2e41d84b6f";
const MULTIPART_CONTENT_TYPE: &str =
    "multipart/form-data; boundary=codex-files-boundary-7f3a9c2e41d84b6f";

pub struct FilesClient<T: HttpTransport> {
    session: EndpointSession<T>,
}

#[derive(Deserialize)]
struct UploadedFile {
    id: String,
}

impl<T: HttpTransport> FilesClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
        }
    }

    pub fn with_telemetry(self, request: Option<Arc<dyn RequestTelemetry>>) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
        }
    }

    /// Uploads `bytes` (a `mime` image) for use in requests (purpose
    /// `user_data`), deleted by the provider `expires_seconds` after (kept
    /// until deleted when `None`); returns its file id.
    pub async fn upload_image(
        &self,
        mime: String,
        bytes: Vec<u8>,
        expires_seconds: Option<u64>,
    ) -> Result<String, ApiError> {
        let body = Bytes::from(upload_body(&mime, &bytes, expires_seconds));
        let resp = self
            .session
            .execute_with(
                Method::POST,
                "files",
                HeaderMap::new(),
                /*body*/ None,
                |req| {
                    req.headers.insert(
                        CONTENT_TYPE,
                        HeaderValue::from_static(MULTIPART_CONTENT_TYPE),
                    );
                    req.body = Some(RequestBody::Raw(body.clone()));
                },
            )
            .await?;
        let file: UploadedFile =
            serde_json::from_slice(&resp.body).map_err(|err| ApiError::Stream(format!(
                "unexpected files API answer: {err}"
            )))?;
        Ok(file.id)
    }
}

/// The MIME type and bytes of a base64 image data URL.
pub fn decode_image_data_url(url: &str) -> Option<(String, Vec<u8>)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    if !mime.to_ascii_lowercase().starts_with("image/") {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .ok()?;
    Some((mime.to_string(), bytes))
}

fn upload_body(mime: &str, bytes: &[u8], expires_seconds: Option<u64>) -> Vec<u8> {
    let mut body = Vec::with_capacity(bytes.len() + 512);
    let mut field = |name: &str, value: &str| {
        body.extend_from_slice(format!("--{MULTIPART_BOUNDARY}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    };
    field("purpose", "user_data");
    if let Some(seconds) = expires_seconds {
        field("expires_after[anchor]", "created_at");
        field("expires_after[seconds]", &seconds.to_string());
    }
    let extension = mime
        .split_once('/')
        .map_or("bin", |(_, subtype)| subtype)
        .replace("jpeg", "jpg");
    body.extend_from_slice(format!("--{MULTIPART_BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"image.{extension}\"\r\n\
             Content-Type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{MULTIPART_BOUNDARY}--\r\n").as_bytes());
    body
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
