//! Fork addition: entry-level deletion of a single memory file.
//!
//! Path handling mirrors [`super::read`]: the path is resolved through
//! `resolve_scoped_path`, which rejects `..`, absolute paths, and hidden
//! components, and symlinks are refused before anything is removed.

use crate::backend::DeleteMemoryRequest;
use crate::backend::DeleteMemoryResponse;
use crate::backend::MemoriesBackendError;

use super::LocalMemoriesBackend;
use super::path::reject_symlink;

pub(super) async fn delete(
    backend: &LocalMemoriesBackend,
    request: DeleteMemoryRequest,
) -> Result<DeleteMemoryResponse, MemoriesBackendError> {
    let path = backend
        .resolve_scoped_path(Some(request.path.as_str()))
        .await?;
    let Some(metadata) = LocalMemoriesBackend::metadata_or_none(&path).await? else {
        return Err(MemoriesBackendError::NotFound { path: request.path });
    };
    reject_symlink(&request.path, &metadata)?;
    if !metadata.is_file() {
        return Err(MemoriesBackendError::NotFile { path: request.path });
    }

    match tokio::fs::remove_file(&path).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(MemoriesBackendError::NotFound { path: request.path });
        }
        Err(err) => return Err(err.into()),
    }

    Ok(DeleteMemoryResponse {
        path: request.path,
        deleted: true,
    })
}
