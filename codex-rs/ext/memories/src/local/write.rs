//! Fork addition: create or overwrite one file under the memory root.
//!
//! Path handling mirrors [`super::delete`]: `resolve_scoped_path` rejects
//! `..`, absolute paths and hidden components, and an existing entry must be a
//! regular file, never a symlink, before it is replaced.

use crate::backend::MemoriesBackendError;
use crate::backend::WriteMemoryRequest;
use crate::backend::WriteMemoryResponse;

use super::LocalMemoriesBackend;
use super::path::reject_symlink;

pub(super) async fn write(
    backend: &LocalMemoriesBackend,
    request: WriteMemoryRequest,
) -> Result<WriteMemoryResponse, MemoriesBackendError> {
    let path = backend
        .resolve_scoped_path(Some(request.path.as_str()))
        .await?;
    if let Some(metadata) = LocalMemoriesBackend::metadata_or_none(&path).await? {
        reject_symlink(&request.path, &metadata)?;
        if !metadata.is_file() {
            return Err(MemoriesBackendError::NotFile { path: request.path });
        }
    } else if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let bytes_written = request.content.len();
    tokio::fs::write(&path, request.content.as_bytes()).await?;

    Ok(WriteMemoryResponse {
        path: request.path,
        bytes_written,
    })
}
