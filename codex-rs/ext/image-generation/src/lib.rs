mod artifact;
mod backend;
mod extension;
mod tool;

// Fork addition: tests for the saved-image hook.
#[cfg(test)]
mod hook_tests;

pub use extension::install;
// Fork addition: host callback after an image is saved.
pub use extension::install_with_saved_image_hook;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use codex_utils_absolute_path::AbsolutePathBuf;

/// Fork addition: an image the tool has just saved, as reported to the host.
#[derive(Debug, Clone)]
pub struct SavedImage {
    pub thread_id: String,
    pub call_id: String,
    pub saved_path: AbsolutePathBuf,
}

/// Fork addition: host callback run after an image is saved and before the tool
/// result goes back to the model.
///
/// Whatever text it returns replaces the model-facing hint, so a host can put
/// the image somewhere the model's own tools reach and say where in that one
/// result, rather than following it up with a second message. `None` keeps the
/// built-in hint.
pub type SavedImageHook =
    Arc<dyn Fn(SavedImage) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

pub(crate) const IMAGE_GEN_NAMESPACE: &str = "image_gen";
pub(crate) const IMAGEGEN_TOOL_NAME: &str = "imagegen";
