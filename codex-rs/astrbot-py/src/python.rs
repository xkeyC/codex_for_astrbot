//! pyo3 surface (feature `python`).
use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use codex_image_generation_extension::SavedImage;
use codex_image_generation_extension::SavedImageHook;

use super::engine::Engine;
use super::engine::EngineOptions;
use super::engine::ReviewRequest;
use super::engine::ThreadParams;
use super::engine::TurnRequest;

/// Codex worker threads run deep async stacks; match the CLI's 16 MiB.
const THREAD_STACK_SIZE_BYTES: usize = 16 * 1024 * 1024;

fn runtime_err(err: anyhow::Error) -> PyErr {
    PyRuntimeError::new_err(format!("{err:#}"))
}

fn parse<T: serde::de::DeserializeOwned>(json: &str) -> PyResult<T> {
    serde_json::from_str(json).map_err(|err| PyValueError::new_err(err.to_string()))
}

#[pyclass(frozen)]
struct Runtime {
    engine: Arc<Engine>,
}

#[pymethods]
impl Runtime {
    /// Test seam: call the registered saved-image hook as Codex would, and
    /// return what it produced. Lets a host verify the Rust -> Python async
    /// round trip without spending an image generation.
    fn _fire_saved_image_hook<'py>(
        &self,
        py: Python<'py>,
        thread_id: String,
        call_id: String,
        saved_path: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        let saved_path = codex_utils_absolute_path::AbsolutePathBuf::try_from(
            std::path::PathBuf::from(saved_path),
        )
        .map_err(|err| PyValueError::new_err(err.to_string()))?;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(engine
                .fire_saved_image_hook(SavedImage {
                    thread_id,
                    call_id,
                    saved_path,
                })
                .await)
        })
    }

    /// Register `async def callback(thread_id, call_id, saved_path) -> str | None`,
    /// awaited after Codex saves a generated image. The string it returns is
    /// what the model sees as the tool result; `None` keeps Codex's own hint.
    /// Must be called from inside a running asyncio loop, which is where the
    /// callback will run. Pass `None` to remove it.
    #[pyo3(signature = (callback))]
    fn set_saved_image_hook(&self, py: Python<'_>, callback: Option<Py<PyAny>>) -> PyResult<()> {
        let Some(callback) = callback else {
            self.engine.set_saved_image_hook(None);
            return Ok(());
        };
        let locals = pyo3_async_runtimes::tokio::get_current_locals(py)?;
        let callback = Arc::new(callback);
        let hook: SavedImageHook = Arc::new(move |image: SavedImage| {
            let callback = Arc::clone(&callback);
            let locals = locals.clone();
            Box::pin(async move {
                let saved_path = image.saved_path.as_path().to_string_lossy().into_owned();
                let pending = Python::attach(|py| {
                    let awaitable =
                        callback
                            .bind(py)
                            .call1((image.thread_id, image.call_id, saved_path))?;
                    pyo3_async_runtimes::into_future_with_locals(&locals, awaitable)
                });
                let result = match pending {
                    Ok(pending) => pending.await,
                    Err(err) => Err(err),
                };
                // The host logs its own failures; an exception that still
                // escapes it only means Codex's built-in hint is used.
                let value = result.ok()?;
                Python::attach(|py| value.bind(py).extract::<Option<String>>().ok().flatten())
            })
        });
        self.engine.set_saved_image_hook(Some(hook));
        Ok(())
    }

    /// Create a runtime. `options_json`: `{"codex_home", "config", "codex_self_exe", "code_mode_host"}`.
    #[staticmethod]
    fn create<'py>(py: Python<'py>, options_json: String) -> PyResult<Bound<'py, PyAny>> {
        let options: EngineOptions = parse(&options_json)?;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let engine = Engine::new(options).await.map_err(runtime_err)?;
            Ok(Runtime {
                engine: Arc::new(engine),
            })
        })
    }

    fn start_thread<'py>(
        &self,
        py: Python<'py>,
        params_json: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let params: ThreadParams = parse(&params_json)?;
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let info = engine.start_thread(params).await.map_err(runtime_err)?;
            Ok(info.to_string())
        })
    }

    fn resume_thread<'py>(
        &self,
        py: Python<'py>,
        params_json: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let params: ThreadParams = parse(&params_json)?;
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let info = engine.resume_thread(params).await.map_err(runtime_err)?;
            Ok(info.to_string())
        })
    }

    fn is_loaded<'py>(&self, py: Python<'py>, thread_id: String) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(engine.is_loaded(&thread_id).await)
        })
    }

    /// JSON `{"model", "model_provider", "total_token_usage": TokenUsage | null}`
    /// for a loaded thread.
    fn thread_usage<'py>(&self, py: Python<'py>, thread_id: String) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let usage = engine.thread_usage(&thread_id).await.map_err(runtime_err)?;
            Ok(usage.to_string())
        })
    }

    /// `request_json`: `{"input": [UserInput], "mode", "expected_turn_id",
    /// "additional_context": {key: {"value", "kind"}}, "dynamic_tools", "model", "effort"}`.
    fn submit_turn<'py>(
        &self,
        py: Python<'py>,
        thread_id: String,
        request_json: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let request: TurnRequest = parse(&request_json)?;
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = engine
                .submit_turn(&thread_id, request)
                .await
                .map_err(runtime_err)?;
            Ok(result.to_string())
        })
    }

    /// Next event of the thread as `{"id", "msg": {"type", ...}}` JSON, or
    /// `None` once the thread has terminated and its events are drained.
    ///
    /// Do not cancel a pending call (e.g. with `asyncio.wait_for`): an event
    /// received after cancellation is dropped.
    fn next_event<'py>(&self, py: Python<'py>, thread_id: String) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            // Resolve the thread first so the wait does not keep the engine alive.
            let thread = engine.thread(&thread_id).await.map_err(runtime_err)?;
            drop(engine);
            let event = Engine::next_event(thread).await.map_err(runtime_err)?;
            Ok(event.map(|event| event.to_string()))
        })
    }

    /// `response_json`: `{"content_items": [...], "success": bool}`.
    fn dynamic_tool_response<'py>(
        &self,
        py: Python<'py>,
        thread_id: String,
        call_id: String,
        response_json: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let response = parse(&response_json)?;
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            engine
                .dynamic_tool_response(&thread_id, call_id, response)
                .await
                .map_err(runtime_err)?;
            Ok(())
        })
    }

    fn set_dynamic_tools<'py>(
        &self,
        py: Python<'py>,
        thread_id: String,
        tools_json: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let tools = parse(&tools_json)?;
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            engine
                .set_dynamic_tools(&thread_id, tools)
                .await
                .map_err(runtime_err)?;
            Ok(())
        })
    }

    /// `request_json`: `{"kind": "exec"|"patch", "id", "turn_id"?, "approved", "reason"?}`.
    fn review_decision<'py>(
        &self,
        py: Python<'py>,
        thread_id: String,
        request_json: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let request: ReviewRequest = parse(&request_json)?;
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            engine
                .review_decision(&thread_id, request)
                .await
                .map_err(runtime_err)?;
            Ok(())
        })
    }

    /// Extract and consolidate memories now (global, then the thread's scope).
    #[pyo3(signature = (thread_id, force = false))]
    fn consolidate_memories<'py>(
        &self,
        py: Python<'py>,
        thread_id: String,
        force: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            engine
                .consolidate_memories(&thread_id, force)
                .await
                .map_err(runtime_err)?;
            Ok(())
        })
    }

    fn interrupt<'py>(&self, py: Python<'py>, thread_id: String) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            engine.interrupt(&thread_id).await.map_err(runtime_err)?;
            Ok(())
        })
    }

    fn shutdown_thread<'py>(
        &self,
        py: Python<'py>,
        thread_id: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            engine
                .shutdown_thread(&thread_id)
                .await
                .map_err(runtime_err)?;
            Ok(())
        })
    }

    /// `{"logged_in", "mode", "email", "account_id", "plan"}` as JSON.
    fn account_status<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(engine.account_status().await.to_string())
        })
    }

    fn login_api_key<'py>(&self, py: Python<'py>, api_key: String) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            engine.login_api_key(&api_key).await.map_err(runtime_err)?;
            Ok(())
        })
    }

    /// Start ChatGPT device-code login: `{"login_id", "verification_url", "user_code"}`.
    fn start_device_login<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let info = engine.start_device_login().await.map_err(runtime_err)?;
            Ok(info.to_string())
        })
    }

    /// `{"status": "pending" | "success" | "failed" | "unknown", "error"?}`.
    fn device_login_status<'py>(
        &self,
        py: Python<'py>,
        login_id: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(engine.device_login_status(&login_id).await.to_string())
        })
    }

    fn cancel_device_login<'py>(
        &self,
        py: Python<'py>,
        login_id: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(engine.cancel_device_login(&login_id).await)
        })
    }

    fn logout<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            engine.logout().await.map_err(runtime_err)
        })
    }

    /// Model presets available to the current account, as a JSON array.
    #[pyo3(signature = (include_hidden = false))]
    fn list_models<'py>(
        &self,
        py: Python<'py>,
        include_hidden: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(engine.list_models(include_hidden).await.to_string())
        })
    }

    fn shutdown<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let engine = Arc::clone(&self.engine);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            engine.shutdown().await.map_err(runtime_err)?;
            Ok(())
        })
    }
}

#[pymodule]
fn codex_astrbot(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder
        .enable_all()
        .thread_stack_size(THREAD_STACK_SIZE_BYTES)
        .thread_name("codex-astrbot");
    pyo3_async_runtimes::tokio::init(builder);
    m.add_class::<Runtime>()?;
    Ok(())
}
