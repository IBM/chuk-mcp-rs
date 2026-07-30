//! Streamable HTTP transport bindings, mirroring the Python
//! `chuk_mcp.transports.http` API (`StreamableHTTPParameters`,
//! `StreamableHTTPTransport`).

use std::collections::HashMap;
use std::sync::Arc;

use pyo3::prelude::*;
use tokio::sync::Mutex;

use chuk_mcp::transports::http::{
    StreamableHttpParameters as CoreParams, StreamableHttpTransport as CoreTransport,
};
use chuk_mcp::transports::Transport;

use crate::streams::{PyReadStream, PyWriteStream};
use crate::to_py_err;

/// Parameters for the Streamable HTTP transport.
#[pyclass(name = "StreamableHTTPParameters", from_py_object)]
#[derive(Clone)]
pub struct PyStreamableHttpParameters {
    pub(crate) inner: CoreParams,
}

#[pymethods]
impl PyStreamableHttpParameters {
    #[new]
    #[pyo3(signature = (
        url,
        timeout = 60.0,
        headers = None,
        bearer_token = None,
        session_id = None,
        user_agent = None,
        enable_streaming = true,
        max_concurrent_requests = 10,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        url: String,
        timeout: f64,
        headers: Option<HashMap<String, String>>,
        bearer_token: Option<String>,
        session_id: Option<String>,
        user_agent: Option<String>,
        enable_streaming: bool,
        max_concurrent_requests: usize,
    ) -> PyResult<Self> {
        let mut inner = CoreParams::new(url).map_err(to_py_err)?;
        inner.timeout = timeout;
        if let Some(headers) = headers {
            inner.headers = headers;
        }
        inner.bearer_token = bearer_token;
        inner.session_id = session_id;
        if let Some(user_agent) = user_agent {
            inner.user_agent = user_agent;
        }
        inner.enable_streaming = enable_streaming;
        inner.max_concurrent_requests = max_concurrent_requests;
        Ok(PyStreamableHttpParameters { inner })
    }

    #[getter]
    fn url(&self) -> &str {
        &self.inner.url
    }
    #[getter]
    fn timeout(&self) -> f64 {
        self.inner.timeout
    }
    #[getter]
    fn session_id(&self) -> Option<&str> {
        self.inner.session_id.as_deref()
    }
    #[getter]
    fn enable_streaming(&self) -> bool {
        self.inner.enable_streaming
    }

    fn __repr__(&self) -> String {
        format!("StreamableHTTPParameters(url={:?})", self.inner.url)
    }
}

/// Streamable HTTP transport. Start it with `async with` (or by awaiting
/// `__aenter__`), then call `get_streams()` for the `(read, write)` pair.
#[pyclass(name = "StreamableHTTPTransport")]
pub struct PyStreamableHttpTransport {
    params: CoreParams,
    transport: Arc<Mutex<Option<CoreTransport>>>,
}

#[pymethods]
impl PyStreamableHttpTransport {
    #[new]
    fn new(parameters: PyStreamableHttpParameters) -> Self {
        PyStreamableHttpTransport {
            params: parameters.inner,
            transport: Arc::new(Mutex::new(None)),
        }
    }

    fn __aenter__<'py>(slf: Py<Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let (params, slot) = {
            let this = slf.borrow(py);
            (this.params.clone(), this.transport.clone())
        };
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let transport = CoreTransport::start(params).map_err(to_py_err)?;
            *slot.lock().await = Some(transport);
            Ok(slf)
        })
    }

    #[pyo3(signature = (*_args))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _args: Bound<'py, pyo3::types::PyTuple>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.close(py)
    }

    /// Get the `(read, write)` stream pair for JSON-RPC communication.
    fn get_streams<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let slot = self.transport.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = slot.lock().await;
            let transport = guard.as_ref().ok_or_else(|| {
                to_py_err(chuk_mcp::McpError::Transport(
                    "Transport not started - use as async context manager".into(),
                ))
            })?;
            let (read, write) = transport.get_streams().await.map_err(to_py_err)?;
            Ok((PyReadStream { inner: read }, PyWriteStream { inner: write }))
        })
    }

    /// The current MCP session id, if the server assigned one.
    fn get_session_id(&self) -> Option<String> {
        self.transport
            .try_lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|t| t.get_session_id()))
    }

    /// Set the negotiated protocol version (no-op for HTTP, kept for API parity).
    fn set_protocol_version(&self, _version: &str) {}

    /// Shut the transport down.
    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let slot = self.transport.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            if let Some(mut transport) = slot.lock().await.take() {
                transport.close().await.map_err(to_py_err)?;
            }
            Ok(())
        })
    }
}

/// Register the HTTP transport classes on the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStreamableHttpParameters>()?;
    m.add_class::<PyStreamableHttpTransport>()?;
    Ok(())
}
