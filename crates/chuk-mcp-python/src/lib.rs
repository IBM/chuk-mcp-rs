//! Python bindings for the chuk-mcp Rust core.
//!
//! Exposes an asyncio-friendly API mirroring the Python `chuk_mcp` package:
//! `StdioParameters`, `connect_to_server`, `MCPClient`, and `MCPServer`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pythonize::{depythonize, pythonize};
use serde_json::Value;
use tokio::sync::Mutex;

use chuk_mcp::client::McpClient as CoreClient;
use chuk_mcp::connect::Connect as CoreConnect;
use chuk_mcp::protocol::era::{EraMode, ProtocolEra};
use chuk_mcp::protocol::types::capabilities::ServerCapabilities as CoreServerCapabilities;
use chuk_mcp::protocol::types::info::ServerInfo;
use chuk_mcp::server::McpServer as CoreServer;
use chuk_mcp::transports::stdio::{StdioParameters as CoreStdioParameters, StdioTransport};
use chuk_mcp::transports::stdio_dual::{stdio_client_dual, StdioDualOptions};

mod arguments;
mod http;
mod server;
mod streams;
mod types;
use arguments::{ResourceParts, ToolParts};

/// The mime type a resource is assumed to serve when none is given.
const DEFAULT_RESOURCE_MIME_TYPE: &str = "text/plain";
use types::{
    PyGetPromptResult, PyPrompt, PyReadResourceResult, PyResource, PyServerCapabilities,
    PyServerInfo, PyTool, PyToolResult,
};

// Exception hierarchy mirroring chuk_mcp: a base McpError with retryable /
// non-retryable / version-mismatch / validation subclasses.
pyo3::create_exception!(chuk_mcp_rs, McpError, pyo3::exceptions::PyException);
pyo3::create_exception!(chuk_mcp_rs, RetryableError, McpError);
pyo3::create_exception!(chuk_mcp_rs, NonRetryableError, McpError);
pyo3::create_exception!(chuk_mcp_rs, VersionMismatchError, McpError);
pyo3::create_exception!(chuk_mcp_rs, ValidationError, McpError);

/// Map a core error to the matching Python exception subclass.
pub(crate) fn to_py_err(e: chuk_mcp::McpError) -> PyErr {
    use chuk_mcp::McpError as E;
    let msg = e.to_string();
    match e {
        E::Retryable { .. } => RetryableError::new_err(msg),
        E::NonRetryable { .. } => NonRetryableError::new_err(msg),
        E::VersionMismatch { .. } => VersionMismatchError::new_err(msg),
        E::Validation { .. } => ValidationError::new_err(msg),
        _ => McpError::new_err(msg),
    }
}

pub(crate) fn json_to_py(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    Ok(pythonize(py, value)?.unbind())
}

pub(crate) fn py_to_json(value: &Bound<'_, PyAny>) -> PyResult<Value> {
    Ok(depythonize(value)?)
}

/// Like [`py_to_json`], but first unwraps Pydantic-style models via
/// `model_dump()` so callers can pass either a dict or a model object.
pub(crate) fn coerce_to_json(value: &Bound<'_, PyAny>) -> PyResult<Value> {
    if let Ok(dump) = value.getattr("model_dump") {
        if dump.is_callable() {
            let dumped = dump.call0()?;
            return py_to_json(&dumped);
        }
    }
    py_to_json(value)
}

/// Parameters for stdio transport.
#[pyclass(name = "StdioParameters", from_py_object)]
#[derive(Clone)]
struct PyStdioParameters {
    inner: CoreStdioParameters,
}

#[pymethods]
impl PyStdioParameters {
    #[new]
    #[pyo3(signature = (command, args=None, env=None))]
    fn new(
        command: String,
        args: Option<Vec<String>>,
        env: Option<HashMap<String, String>>,
    ) -> Self {
        let mut params = CoreStdioParameters::new(command, args.unwrap_or_default());
        params.env = env;
        PyStdioParameters { inner: params }
    }

    #[getter]
    fn command(&self) -> &str {
        &self.inner.command
    }

    #[getter]
    fn args(&self) -> Vec<String> {
        self.inner.args.clone()
    }

    #[getter]
    fn env(&self) -> Option<HashMap<String, String>> {
        self.inner.env.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "StdioParameters(command={:?}, args={:?})",
            self.inner.command, self.inner.args
        )
    }
}

impl PyStdioParameters {
    pub(crate) fn into_inner(self) -> CoreStdioParameters {
        self.inner
    }
}

/// High-level MCP client backed by the Rust core.
#[pyclass(name = "MCPClient")]
struct PyMcpClient {
    inner: Arc<Mutex<Option<CoreClient>>>,
    server_info: Option<ServerInfo>,
    capabilities: Option<CoreServerCapabilities>,
    /// The negotiated protocol era: "legacy" or "2026-07-28".
    era: String,
    /// The negotiated protocol version, if known.
    protocol_version: Option<String>,
}

impl PyMcpClient {
    /// Wrap a settled core client, reading the era and version off it rather
    /// than asking the caller to restate what the handshake already decided.
    fn wrap(client: CoreClient) -> PyMcpClient {
        let server_info = client.server_info().cloned();
        let capabilities = client.capabilities().cloned();
        let era = client.era().unwrap_or(ProtocolEra::Legacy).to_string();
        let protocol_version = client.protocol_version().map(str::to_string);

        PyMcpClient {
            inner: Arc::new(Mutex::new(Some(client))),
            server_info,
            capabilities,
            era,
            protocol_version,
        }
    }

    fn client(&self) -> Arc<Mutex<Option<CoreClient>>> {
        self.inner.clone()
    }
}

macro_rules! with_client {
    ($client:ident, $body:expr) => {{
        let guard = $client.lock().await;
        match guard.as_ref() {
            Some($client) => $body,
            None => Err(chuk_mcp::McpError::Transport("Client is closed".into())),
        }
    }};
}

#[pymethods]
impl PyMcpClient {
    /// Server info from initialization (a ServerInfo, or None).
    #[getter]
    fn server_info(&self) -> Option<PyServerInfo> {
        self.server_info.clone().map(PyServerInfo::from)
    }

    /// Server capabilities from initialization (a ServerCapabilities, or None).
    #[getter]
    fn capabilities(&self) -> Option<PyServerCapabilities> {
        self.capabilities.clone().map(PyServerCapabilities::from)
    }

    /// The negotiated protocol era: "legacy" or "2026-07-28".
    #[getter]
    fn era(&self) -> String {
        self.era.clone()
    }

    /// The negotiated protocol version string, if known.
    #[getter]
    fn protocol_version(&self) -> Option<String> {
        self.protocol_version.clone()
    }

    /// List available tools (list of Tool objects).
    fn list_tools<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let tools = with_client!(client, client.list_tools().await).map_err(to_py_err)?;
            Ok(tools.into_iter().map(PyTool::from).collect::<Vec<_>>())
        })
    }

    /// Call a tool; returns a ToolResult.
    #[pyo3(signature = (name, arguments=None))]
    fn call_tool<'py>(
        &self,
        py: Python<'py>,
        name: String,
        arguments: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client();
        let args: Value = match arguments {
            Some(args) => py_to_json(&args)?,
            None => Value::Object(serde_json::Map::new()),
        };
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result =
                with_client!(client, client.call_tool(&name, args).await).map_err(to_py_err)?;
            Ok(PyToolResult::from(result))
        })
    }

    /// List available resources (list of Resource objects).
    fn list_resources<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let resources =
                with_client!(client, client.list_resources().await).map_err(to_py_err)?;
            Ok(resources
                .into_iter()
                .map(PyResource::from)
                .collect::<Vec<_>>())
        })
    }

    /// Read a resource by URI; returns a ReadResourceResult.
    fn read_resource<'py>(&self, py: Python<'py>, uri: String) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result =
                with_client!(client, client.read_resource(&uri).await).map_err(to_py_err)?;
            Ok(PyReadResourceResult::from(result))
        })
    }

    /// List available prompts (list of Prompt objects).
    fn list_prompts<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let prompts = with_client!(client, client.list_prompts().await).map_err(to_py_err)?;
            Ok(prompts.into_iter().map(PyPrompt::from).collect::<Vec<_>>())
        })
    }

    /// Get a prompt by name; returns a GetPromptResult.
    #[pyo3(signature = (name, arguments=None))]
    fn get_prompt<'py>(
        &self,
        py: Python<'py>,
        name: String,
        arguments: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client();
        let args: Option<Value> = match arguments {
            Some(args) => Some(py_to_json(&args)?),
            None => None,
        };
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result =
                with_client!(client, client.get_prompt(&name, args).await).map_err(to_py_err)?;
            Ok(PyGetPromptResult::from(result))
        })
    }

    /// Ping the server; resolves to True/False.
    fn ping<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let ok = {
                let guard = client.lock().await;
                match guard.as_ref() {
                    Some(client) => client.ping().await,
                    None => false,
                }
            };
            Ok(ok)
        })
    }

    /// The raw `(read, write)` stream pair, for driving the lower-level `send_*`
    /// helpers directly. The era is already settled and `_meta` injection (if
    /// the peer is modern) lives in the transport, so the same `send_*` calls
    /// work whichever era this connection negotiated. Resolves to a
    /// `(ReadStream, WriteStream)` tuple.
    fn raw_streams<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = client.lock().await;
            let core = guard.as_ref().ok_or_else(|| {
                to_py_err(chuk_mcp::McpError::Transport("client is closed".into()))
            })?;
            let (read, write) = core.raw_streams().map_err(to_py_err)?;
            Ok((
                crate::streams::PyReadStream { inner: read },
                crate::streams::PyWriteStream { inner: write },
            ))
        })
    }

    /// Shut down the underlying transport.
    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = client.lock().await;
            if let Some(mut client) = guard.take() {
                client.close().await.map_err(to_py_err)?;
            }
            Ok(())
        })
    }

    /// Async context manager support.
    fn __aenter__<'py>(slf: Py<Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(slf) })
    }

    #[pyo3(signature = (*_args))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _args: Bound<'py, pyo3::types::PyTuple>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.close(py)
    }
}

/// Connect to an MCP server — a URL or a command line — in one call.
///
/// `"http://…"` / `"https://…"` is a Streamable HTTP endpoint; anything else is
/// a command line to spawn, split on whitespace. The protocol era is detected
/// and its handshake completed, so the returned MCPClient is ready to use and
/// reports the negotiated `.era` and `.protocol_version`.
///
/// ```text
/// client = await connect("https://example.com/mcp", bearer_token=token)
/// client = await connect("python server.py")
/// ```
///
/// `era` pins the protocol generation instead of detecting it: `"auto"`
/// (default), `"legacy"` or `"2026-07-28"`. `bearer_token` and `headers` apply
/// to HTTP targets, `env` to subprocess targets; `timeout` is in seconds and
/// bounds the probe or handshake.
#[pyfunction]
#[pyo3(signature = (
    target,
    era="auto",
    bearer_token=None,
    headers=None,
    env=None,
    timeout=None,
    credential_context=None,
))]
#[allow(clippy::too_many_arguments)]
fn connect<'py>(
    py: Python<'py>,
    target: String,
    era: &str,
    bearer_token: Option<String>,
    headers: Option<HashMap<String, String>>,
    env: Option<HashMap<String, String>>,
    timeout: Option<f64>,
    credential_context: Option<String>,
) -> PyResult<Bound<'py, PyAny>> {
    let mode: EraMode = era.parse().map_err(to_py_err)?;

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let mut builder = CoreConnect::to(target).era(mode);
        if let Some(token) = bearer_token {
            builder = builder.bearer_token(token);
        }
        for (name, value) in headers.unwrap_or_default() {
            builder = builder.header(name, value);
        }
        if let Some(env) = env {
            builder = builder.env(env);
        }
        if let Some(seconds) = timeout {
            builder = builder.timeout(Duration::from_secs_f64(seconds));
        }
        if let Some(context) = credential_context {
            builder = builder.credential_context(context);
        }

        let client = builder.connect().await.map_err(to_py_err)?;
        Ok(PyMcpClient::wrap(client))
    })
}

/// Connect to an MCP server over stdio with automatic initialization.
///
/// Prefer [`connect`], which also handles HTTP targets and detects the era.
/// Returns an MCPClient (also usable as an async context manager).
#[pyfunction]
fn connect_to_server<'py>(
    py: Python<'py>,
    parameters: PyStdioParameters,
) -> PyResult<Bound<'py, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let transport = StdioTransport::start(parameters.inner)
            .await
            .map_err(to_py_err)?;
        let mut client = CoreClient::new(transport);
        client.initialize().await.map_err(to_py_err)?;
        Ok(PyMcpClient::wrap(client))
    })
}

/// Connect to an MCP server over stdio with dual-era detection.
///
/// Probes with `server/discover`; a modern peer is driven with the stateless
/// `2026-07-28` protocol (per-request `_meta`, injected by the transport) and a
/// legacy peer falls back to the `initialize` handshake. `mode` is `"auto"`
/// (default), `"legacy"`, or `"2026-07-28"`. The returned client exposes the
/// negotiated `.era` and `.protocol_version`.
#[pyfunction]
#[pyo3(signature = (parameters, mode="auto"))]
fn connect_dual_stdio<'py>(
    py: Python<'py>,
    parameters: PyStdioParameters,
    mode: &str,
) -> PyResult<Bound<'py, PyAny>> {
    let era_mode: EraMode = mode.parse().map_err(to_py_err)?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let options = StdioDualOptions {
            mode: era_mode,
            ..StdioDualOptions::default()
        };
        let conn = stdio_client_dual(parameters.inner, options)
            .await
            .map_err(to_py_err)?;

        Ok(PyMcpClient::wrap(CoreClient::from_profile(
            conn.transport,
            conn.read,
            conn.write,
            conn.profile,
        )))
    })
}

/// High-level MCP server backed by the Rust core. Register async Python
/// handlers, then serve with `run_stdio()`.
#[pyclass(name = "MCPServer")]
struct PyMcpServer {
    inner: Arc<Mutex<Option<CoreServer>>>,
}

#[pymethods]
impl PyMcpServer {
    #[new]
    #[pyo3(signature = (name, version="1.0.0", capabilities=None))]
    fn new(name: &str, version: &str, capabilities: Option<Bound<'_, PyAny>>) -> PyResult<Self> {
        let caps =
            match capabilities {
                Some(c) => {
                    let value = coerce_to_json(&c)?;
                    Some(serde_json::from_value(value).map_err(|e| {
                        PyRuntimeError::new_err(format!("invalid capabilities: {e}"))
                    })?)
                }
                None => None,
            };
        Ok(PyMcpServer {
            inner: Arc::new(Mutex::new(Some(CoreServer::new(name, version, caps)))),
        })
    }

    /// The server's protocol handler, for registering custom method handlers
    /// (`server.protocol_handler.register_method(...)`).
    #[getter]
    fn protocol_handler(&self) -> server::PyProtocolHandler {
        server::PyProtocolHandler {
            server: self.inner.clone(),
        }
    }

    /// Register a tool.
    ///
    /// `handler` is an async callable taking the tool's arguments as keywords
    /// and returning a str or any JSON-serializable value; `schema` is the
    /// tool's JSON Schema as a mapping; `description` is a string.
    ///
    /// The three may be given in **any order**. The historical `chuk_mcp`
    /// order is `(name, handler, schema, description)` and the Rust core's is
    /// `(name, schema, description, handler)`; rather than make one of them
    /// wrong, both are accepted — a callable can only be the handler, a
    /// mapping can only be the schema, and a string can only be the
    /// description, so there is nothing to guess at.
    #[pyo3(signature = (name, handler=None, schema=None, description=None))]
    fn register_tool(
        &self,
        py: Python<'_>,
        name: &str,
        handler: Option<Bound<'_, PyAny>>,
        schema: Option<Bound<'_, PyAny>>,
        description: Option<Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let parts = ToolParts::resolve([handler, schema, description])?;
        let schema: Value = py_to_json(&parts.schema)?;
        let description = parts.description;
        let description = description.as_str();
        let handler = Arc::new(parts.handler.unbind());

        let mut guard = self.inner.blocking_lock_owned_or_py(py)?;
        let server = guard
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Server already running"))?;

        server.register_tool(name, schema, description, move |args| {
            let handler = handler.clone();
            async move {
                // Call the Python async handler and await its coroutine.
                // Arguments are passed as keyword arguments (handler(**arguments)),
                // matching the historical chuk_mcp.MCPServer convention
                // (e.g. `async def greet(name): ...`). Falls back to a single
                // positional argument if the payload isn't an object.
                let future = Python::attach(|py| -> PyResult<_> {
                    let args_obj = pythonize(py, &args)?;
                    let coroutine = match args_obj.cast::<PyDict>() {
                        Ok(kwargs) => handler.bind(py).call((), Some(kwargs))?,
                        Err(_) => handler.bind(py).call1((args_obj,))?,
                    };
                    pyo3_async_runtimes::tokio::into_future(coroutine)
                })
                .map_err(|e| e.to_string())?;

                let result = future.await.map_err(|e| e.to_string())?;
                Python::attach(|py| py_to_json(result.bind(py))).map_err(|e| e.to_string())
            }
        });
        Ok(())
    }

    /// Register a resource. `handler` is an async callable returning a str.
    ///
    /// The handler may appear in any position — `chuk_mcp` puts it second and
    /// the Rust core puts it last — because only it is callable. The remaining
    /// strings fill `name`, `description` and `mime_type` in the order given.
    #[pyo3(signature = (uri, handler=None, name=None, description=None, mime_type=None))]
    fn register_resource(
        &self,
        py: Python<'_>,
        uri: &str,
        handler: Option<Bound<'_, PyAny>>,
        name: Option<Bound<'_, PyAny>>,
        description: Option<Bound<'_, PyAny>>,
        mime_type: Option<Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let parts = ResourceParts::resolve(
            [handler, name, description, mime_type],
            DEFAULT_RESOURCE_MIME_TYPE,
        )?;
        let (name, description, mime_type) = (parts.name, parts.description, parts.mime_type);
        let handler = Arc::new(parts.handler.unbind());

        let mut guard = self.inner.blocking_lock_owned_or_py(py)?;
        let server = guard
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Server already running"))?;

        server.register_resource(uri, &name, &description, &mime_type, move || {
            let handler = handler.clone();
            async move {
                let future = Python::attach(|py| -> PyResult<_> {
                    let coroutine = handler.bind(py).call0()?;
                    pyo3_async_runtimes::tokio::into_future(coroutine)
                })
                .map_err(|e| e.to_string())?;

                let result = future.await.map_err(|e| e.to_string())?;
                Python::attach(|py| result.bind(py).extract::<String>()).map_err(|e| e.to_string())
            }
        });
        Ok(())
    }

    /// Serve over stdio until stdin closes.
    fn run_stdio<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = inner.lock().await;
            let server = guard
                .as_ref()
                .ok_or_else(|| PyRuntimeError::new_err("Server already running"))?;
            server.run_stdio().await.map_err(to_py_err)?;
            Ok(())
        })
    }
}

/// Helper trait: acquire a tokio mutex from sync Python context.
/// Registration happens before the server runs, so the lock is uncontended;
/// try_lock keeps this non-blocking.
trait BlockingLock<T> {
    fn blocking_lock_owned_or_py(
        &self,
        py: Python<'_>,
    ) -> PyResult<tokio::sync::OwnedMutexGuard<T>>;
}

impl<T> BlockingLock<T> for Arc<Mutex<T>> {
    fn blocking_lock_owned_or_py(
        &self,
        _py: Python<'_>,
    ) -> PyResult<tokio::sync::OwnedMutexGuard<T>> {
        self.clone()
            .try_lock_owned()
            .map_err(|_| PyRuntimeError::new_err("Server is busy"))
    }
}

/// Supported MCP protocol versions (newest first).
#[pyfunction]
fn supported_versions(py: Python<'_>) -> PyResult<Py<PyList>> {
    Ok(PyList::new(py, chuk_mcp::protocol::versioning::SUPPORTED_VERSIONS)?.unbind())
}

/// The version of the underlying Rust core.
#[pyfunction]
fn core_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// A safe default environment for spawned stdio subprocesses.
#[pyfunction]
fn get_default_environment() -> HashMap<String, String> {
    chuk_mcp::transports::stdio::get_default_environment()
}

#[pymodule]
fn chuk_mcp_rs(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStdioParameters>()?;
    m.add_class::<PyMcpClient>()?;
    m.add_class::<PyMcpServer>()?;
    types::register(m)?;
    streams::register(m)?;
    http::register(m)?;
    server::register(m)?;
    m.add_function(wrap_pyfunction!(connect, m)?)?;
    m.add_function(wrap_pyfunction!(connect_to_server, m)?)?;
    m.add_function(wrap_pyfunction!(supported_versions, m)?)?;
    m.add_function(wrap_pyfunction!(connect_dual_stdio, m)?)?;
    m.add_function(wrap_pyfunction!(core_version, m)?)?;
    m.add_function(wrap_pyfunction!(get_default_environment, m)?)?;

    m.add("McpError", py.get_type::<McpError>())?;
    m.add("RetryableError", py.get_type::<RetryableError>())?;
    m.add("NonRetryableError", py.get_type::<NonRetryableError>())?;
    m.add(
        "VersionMismatchError",
        py.get_type::<VersionMismatchError>(),
    )?;
    m.add("ValidationError", py.get_type::<ValidationError>())?;
    m.add(
        "CURRENT_VERSION",
        chuk_mcp::protocol::versioning::CURRENT_VERSION,
    )?;
    m.add(
        "LATEST_LEGACY_VERSION",
        chuk_mcp::protocol::versioning::LATEST_LEGACY_VERSION,
    )?;
    Ok(())
}
