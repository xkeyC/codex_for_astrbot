pub use codex_protocol::models::DEFAULT_IMAGE_DETAIL;
pub use codex_protocol::models::ImageDetail;
use serde::Deserialize;
use serde::Serialize;

/// Fork addition: a nested tool result object may carry the images it
/// returned under this key (data URLs). The runtime shows them to the model
/// in the cell's output, as `image()` would, and takes the key out before
/// the script sees the result, so scripts never hold the base64 text.
pub const ATTACH_IMAGES_KEY: &str = "__codex_attach_images";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FunctionCallOutputContentItem {
    InputText {
        text: String,
    },
    InputImage {
        image_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
    InputAudio {
        audio_url: String,
    },
}
