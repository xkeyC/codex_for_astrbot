//! Fork addition: the memory-file backend a consolidation agent maintains.
//!
//! It is [`LocalMemoriesBackend`] rooted at the agent's `cwd`, with one
//! difference: the consolidation prompt names its files by absolute path
//! (`<memory root>/memory_summary.md`), while the backend takes paths relative
//! to the root and rejects absolute ones. Rather than reword a prompt that is
//! upstream's, accept either spelling here.

use std::path::Path;
use std::path::PathBuf;

use crate::backend::AddAdHocMemoryNoteRequest;
use crate::backend::AddAdHocMemoryNoteResponse;
use crate::backend::DeleteMemoryRequest;
use crate::backend::DeleteMemoryResponse;
use crate::backend::ListMemoriesRequest;
use crate::backend::ListMemoriesResponse;
use crate::backend::MemoriesBackend;
use crate::backend::MemoriesBackendError;
use crate::backend::ReadMemoryRequest;
use crate::backend::ReadMemoryResponse;
use crate::backend::SearchMemoriesRequest;
use crate::backend::SearchMemoriesResponse;
use crate::backend::WriteMemoryRequest;
use crate::backend::WriteMemoryResponse;
use crate::local::LocalMemoriesBackend;

#[derive(Debug, Clone)]
pub(crate) struct MaintenanceBackend {
    root: PathBuf,
    inner: LocalMemoriesBackend,
}

impl MaintenanceBackend {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            inner: LocalMemoriesBackend::from_memory_root(root.clone()),
            root,
        }
    }

    /// Returns `path` relative to the memory root, leaving anything that is
    /// not under it untouched so the backend can reject it as it normally
    /// would.
    fn relative(&self, path: &str) -> String {
        let candidate = Path::new(path);
        if !candidate.is_absolute() {
            return path.to_string();
        }
        match candidate.strip_prefix(&self.root) {
            Ok(relative) => relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
            Err(_) => path.to_string(),
        }
    }
}

impl MemoriesBackend for MaintenanceBackend {
    async fn add_ad_hoc_note(
        &self,
        request: AddAdHocMemoryNoteRequest,
    ) -> Result<AddAdHocMemoryNoteResponse, MemoriesBackendError> {
        self.inner.add_ad_hoc_note(request).await
    }

    async fn list(
        &self,
        request: ListMemoriesRequest,
    ) -> Result<ListMemoriesResponse, MemoriesBackendError> {
        let path = request.path.as_deref().map(|path| self.relative(path));
        self.inner
            .list(ListMemoriesRequest { path, ..request })
            .await
    }

    async fn read(
        &self,
        request: ReadMemoryRequest,
    ) -> Result<ReadMemoryResponse, MemoriesBackendError> {
        let path = self.relative(request.path.as_str());
        self.inner.read(ReadMemoryRequest { path, ..request }).await
    }

    async fn search(
        &self,
        request: SearchMemoriesRequest,
    ) -> Result<SearchMemoriesResponse, MemoriesBackendError> {
        self.inner.search(request).await
    }

    async fn delete(
        &self,
        request: DeleteMemoryRequest,
    ) -> Result<DeleteMemoryResponse, MemoriesBackendError> {
        let path = self.relative(request.path.as_str());
        self.inner.delete(DeleteMemoryRequest { path }).await
    }

    async fn write(
        &self,
        request: WriteMemoryRequest,
    ) -> Result<WriteMemoryResponse, MemoriesBackendError> {
        let path = self.relative(request.path.as_str());
        self.inner
            .write(WriteMemoryRequest {
                path,
                content: request.content,
            })
            .await
    }
}
