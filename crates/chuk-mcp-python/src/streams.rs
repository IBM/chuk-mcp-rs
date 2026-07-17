//! Low-level functional API: opaque stream handles, a `stdio_client` async
//! context manager, and the `send_*` free functions, mirroring the Python
//! `chuk_mcp` message layer (`async with stdio_client(...) as (read, write)`).

use std::sync::Arc;
use std::time::Duration;

use pyo3::prelude::*;
use serde_json::Value;
use tokio::sync::Mutex;

use chuk_mcp::protocol::json_rpc::RequestId;
use chuk_mcp::protocol::messages::initialize::{
    send_initialize_with_options, send_initialized_notification as core_initialized_notification,
    InitializeOptions,
};
use chuk_mcp::protocol::messages::notifications::{
    send_cancelled_notification as core_cancelled, send_progress_notification as core_progress,
    send_roots_list_changed_notification as core_roots_changed,
};
use chuk_mcp::protocol::messages::ping::send_ping as core_send_ping;
use chuk_mcp::protocol::messages::prompts::{
    send_prompts_get as core_prompts_get, send_prompts_list as core_prompts_list,
};
use chuk_mcp::protocol::messages::resources::{
    send_resources_list as core_resources_list, send_resources_read as core_resources_read,
};
use chuk_mcp::protocol::messages::resources::{
    send_resources_subscribe as core_subscribe, send_resources_unsubscribe as core_unsubscribe,
};
use chuk_mcp::protocol::messages::roots::send_roots_list as core_roots_list;
use chuk_mcp::protocol::messages::send_message::send_message as core_send_message;
use chuk_mcp::protocol::messages::send_message::{ReadStream, WriteStream};
use chuk_mcp::protocol::messages::tools::{
    send_tools_call as core_tools_call, send_tools_list as core_tools_list,
};
use chuk_mcp::transports::stdio::{StdioParameters as CoreStdioParameters, StdioTransport};
use chuk_mcp::transports::Transport;

use crate::types::{
    PyGetPromptResult, PyInitializeResult, PyListPromptsResult, PyListResourcesResult,
    PyListToolsResult, PyReadResourceResult, PyToolResult,
};
use crate::{json_to_py, py_to_json, to_py_err};

/// Opaque handle to a transport's inbound message stream.
#[pyclass(name = "ReadStream", frozen)]
#[derive(Clone)]
pub struct PyReadStream {
    pub(crate) inner: ReadStream,
}

/// Opaque handle to a transport's outbound message stream.
#[pyclass(name = "WriteStream", frozen)]
#[derive(Clone)]
pub struct PyWriteStream {
    pub(crate) inner: WriteStream,
}

/// Async context manager wrapping a stdio transport, yielding `(read, write)`.
#[pyclass(name = "StdioClient")]
pub struct PyStdioClient {
    params: CoreStdioParameters,
    transport: Arc<Mutex<Option<StdioTransport>>>,
}

#[pymethods]
impl PyStdioClient {
    fn __aenter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let params = self.params.clone();
        let transport_slot = self.transport.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let transport = StdioTransport::start(params).await.map_err(to_py_err)?;
            let (read, write) = transport.get_streams().await.map_err(to_py_err)?;
            *transport_slot.lock().await = Some(transport);
            Ok((PyReadStream { inner: read }, PyWriteStream { inner: write }))
        })
    }

    #[pyo3(signature = (*_args))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _args: Bound<'py, pyo3::types::PyTuple>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let transport_slot = self.transport.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            if let Some(mut transport) = transport_slot.lock().await.take() {
                transport.close().await.map_err(to_py_err)?;
            }
            Ok(false) // don't suppress exceptions
        })
    }
}

/// Create a stdio client context manager: `async with stdio_client(params) as
/// (read, write): ...`.
#[pyfunction]
pub fn stdio_client(parameters: crate::PyStdioParameters) -> PyStdioClient {
    PyStdioClient {
        params: parameters.into_inner(),
        transport: Arc::new(Mutex::new(None)),
    }
}

/// Perform the initialization handshake on a stream pair.
#[pyfunction]
#[pyo3(signature = (read, write, timeout=None, supported_versions=None, preferred_version=None))]
pub fn send_initialize<'py>(
    py: Python<'py>,
    read: PyReadStream,
    write: PyWriteStream,
    timeout: Option<f64>,
    supported_versions: Option<Vec<String>>,
    preferred_version: Option<String>,
) -> PyResult<Bound<'py, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let options = InitializeOptions {
            timeout: timeout.map(Duration::from_secs_f64),
            supported_versions,
            preferred_version,
            ..Default::default()
        };
        let result = send_initialize_with_options(&read.inner, &write.inner, options)
            .await
            .map_err(to_py_err)?;
        Ok(PyInitializeResult::from(result))
    })
}

/// List tools on a stream pair.
#[pyfunction]
#[pyo3(signature = (read, write, cursor=None, timeout=None))]
pub fn send_tools_list<'py>(
    py: Python<'py>,
    read: PyReadStream,
    write: PyWriteStream,
    cursor: Option<String>,
    timeout: Option<f64>,
) -> PyResult<Bound<'py, PyAny>> {
    let _ = timeout;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = core_tools_list(&read.inner, &write.inner, cursor.as_deref())
            .await
            .map_err(to_py_err)?;
        Ok(PyListToolsResult::from(result))
    })
}

/// Call a tool on a stream pair.
#[pyfunction]
#[pyo3(signature = (read, write, name, arguments=None, timeout=None))]
pub fn send_tools_call<'py>(
    py: Python<'py>,
    read: PyReadStream,
    write: PyWriteStream,
    name: String,
    arguments: Option<Bound<'py, PyAny>>,
    timeout: Option<f64>,
) -> PyResult<Bound<'py, PyAny>> {
    let _ = timeout;
    let args: Value = match arguments {
        Some(args) => py_to_json(&args)?,
        None => Value::Object(serde_json::Map::new()),
    };
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = core_tools_call(&read.inner, &write.inner, &name, args)
            .await
            .map_err(to_py_err)?;
        Ok(PyToolResult::from(result))
    })
}

/// List resources on a stream pair.
#[pyfunction]
#[pyo3(signature = (read, write, cursor=None, timeout=None))]
pub fn send_resources_list<'py>(
    py: Python<'py>,
    read: PyReadStream,
    write: PyWriteStream,
    cursor: Option<String>,
    timeout: Option<f64>,
) -> PyResult<Bound<'py, PyAny>> {
    let _ = timeout;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = core_resources_list(&read.inner, &write.inner, cursor.as_deref())
            .await
            .map_err(to_py_err)?;
        Ok(PyListResourcesResult::from(result))
    })
}

/// Read a resource on a stream pair.
#[pyfunction]
#[pyo3(signature = (read, write, uri, timeout=None))]
pub fn send_resources_read<'py>(
    py: Python<'py>,
    read: PyReadStream,
    write: PyWriteStream,
    uri: String,
    timeout: Option<f64>,
) -> PyResult<Bound<'py, PyAny>> {
    let _ = timeout;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = core_resources_read(&read.inner, &write.inner, &uri)
            .await
            .map_err(to_py_err)?;
        Ok(PyReadResourceResult::from(result))
    })
}

/// List prompts on a stream pair.
#[pyfunction]
#[pyo3(signature = (read, write, cursor=None, timeout=None))]
pub fn send_prompts_list<'py>(
    py: Python<'py>,
    read: PyReadStream,
    write: PyWriteStream,
    cursor: Option<String>,
    timeout: Option<f64>,
) -> PyResult<Bound<'py, PyAny>> {
    let _ = timeout;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = core_prompts_list(&read.inner, &write.inner, cursor.as_deref())
            .await
            .map_err(to_py_err)?;
        Ok(PyListPromptsResult::from(result))
    })
}

/// Get a prompt on a stream pair.
#[pyfunction]
#[pyo3(signature = (read, write, name, arguments=None, timeout=None))]
pub fn send_prompts_get<'py>(
    py: Python<'py>,
    read: PyReadStream,
    write: PyWriteStream,
    name: String,
    arguments: Option<Bound<'py, PyAny>>,
    timeout: Option<f64>,
) -> PyResult<Bound<'py, PyAny>> {
    let _ = timeout;
    let args: Option<Value> = match arguments {
        Some(args) => Some(py_to_json(&args)?),
        None => None,
    };
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = core_prompts_get(&read.inner, &write.inner, &name, args)
            .await
            .map_err(to_py_err)?;
        Ok(PyGetPromptResult::from(result))
    })
}

/// Ping on a stream pair; resolves to True/False.
#[pyfunction]
#[pyo3(signature = (read, write, timeout=None))]
pub fn send_ping<'py>(
    py: Python<'py>,
    read: PyReadStream,
    write: PyWriteStream,
    timeout: Option<f64>,
) -> PyResult<Bound<'py, PyAny>> {
    let _ = timeout;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        Ok(core_send_ping(&read.inner, &write.inner).await)
    })
}

/// Generic low-level JSON-RPC send: returns the raw `result` value.
#[pyfunction]
#[pyo3(signature = (read, write, method, params=None, timeout=None))]
pub fn send_message<'py>(
    py: Python<'py>,
    read: PyReadStream,
    write: PyWriteStream,
    method: String,
    params: Option<Bound<'py, PyAny>>,
    timeout: Option<f64>,
) -> PyResult<Bound<'py, PyAny>> {
    let _ = timeout;
    let params: Option<Value> = match params {
        Some(p) => Some(py_to_json(&p)?),
        None => None,
    };
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = core_send_message(&read.inner, &write.inner, &method, params)
            .await
            .map_err(to_py_err)?;
        Python::with_gil(|py| json_to_py(py, &result))
    })
}

/// Send the `notifications/initialized` notification on a stream pair.
#[pyfunction]
pub fn send_initialized_notification<'py>(
    py: Python<'py>,
    write: PyWriteStream,
) -> PyResult<Bound<'py, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        core_initialized_notification(&write.inner)
            .await
            .map_err(to_py_err)?;
        Ok(())
    })
}

fn to_request_id(value: &Bound<'_, PyAny>) -> PyResult<RequestId> {
    if let Ok(n) = value.extract::<i64>() {
        Ok(RequestId::Num(n))
    } else if let Ok(s) = value.extract::<String>() {
        Ok(RequestId::Str(s))
    } else {
        Err(pyo3::exceptions::PyValueError::new_err(
            "token/id must be an int or str",
        ))
    }
}

/// Send a `notifications/progress` notification.
#[pyfunction]
#[pyo3(signature = (write, progress_token, progress, total=None, message=None))]
pub fn send_progress_notification<'py>(
    py: Python<'py>,
    write: PyWriteStream,
    progress_token: Bound<'py, PyAny>,
    progress: f64,
    total: Option<f64>,
    message: Option<String>,
) -> PyResult<Bound<'py, PyAny>> {
    let token = to_request_id(&progress_token)?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        core_progress(&write.inner, token, progress, total, message.as_deref())
            .await
            .map_err(to_py_err)?;
        Ok(())
    })
}

/// Send a `notifications/cancelled` notification.
#[pyfunction]
#[pyo3(signature = (write, request_id, reason=None))]
pub fn send_cancelled_notification<'py>(
    py: Python<'py>,
    write: PyWriteStream,
    request_id: Bound<'py, PyAny>,
    reason: Option<String>,
) -> PyResult<Bound<'py, PyAny>> {
    let id = to_request_id(&request_id)?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        core_cancelled(&write.inner, id, reason.as_deref())
            .await
            .map_err(to_py_err)?;
        Ok(())
    })
}

/// Send a `notifications/roots/list_changed` notification.
#[pyfunction]
pub fn send_roots_list_changed_notification<'py>(
    py: Python<'py>,
    write: PyWriteStream,
) -> PyResult<Bound<'py, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        core_roots_changed(&write.inner).await.map_err(to_py_err)?;
        Ok(())
    })
}

/// Send a `roots/list` request; returns the raw result value.
#[pyfunction]
#[pyo3(signature = (read, write, timeout=None))]
pub fn send_roots_list<'py>(
    py: Python<'py>,
    read: PyReadStream,
    write: PyWriteStream,
    timeout: Option<f64>,
) -> PyResult<Bound<'py, PyAny>> {
    let _ = timeout;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = core_roots_list(&read.inner, &write.inner)
            .await
            .map_err(to_py_err)?;
        Python::with_gil(|py| json_to_py(py, &serde_json::to_value(result).unwrap()))
    })
}

/// Subscribe to updates for a resource; resolves to True/False.
#[pyfunction]
#[pyo3(signature = (read, write, uri, timeout=None))]
pub fn send_resources_subscribe<'py>(
    py: Python<'py>,
    read: PyReadStream,
    write: PyWriteStream,
    uri: String,
    timeout: Option<f64>,
) -> PyResult<Bound<'py, PyAny>> {
    let _ = timeout;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        Ok(core_subscribe(&read.inner, &write.inner, &uri).await)
    })
}

/// Unsubscribe from updates for a resource; resolves to True/False.
#[pyfunction]
#[pyo3(signature = (read, write, uri, timeout=None))]
pub fn send_resources_unsubscribe<'py>(
    py: Python<'py>,
    read: PyReadStream,
    write: PyWriteStream,
    uri: String,
    timeout: Option<f64>,
) -> PyResult<Bound<'py, PyAny>> {
    let _ = timeout;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        Ok(core_unsubscribe(&read.inner, &write.inner, &uri).await)
    })
}

/// Register the low-level stream API on the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyReadStream>()?;
    m.add_class::<PyWriteStream>()?;
    m.add_class::<PyStdioClient>()?;
    m.add_function(wrap_pyfunction!(stdio_client, m)?)?;
    m.add_function(wrap_pyfunction!(send_initialize, m)?)?;
    m.add_function(wrap_pyfunction!(send_tools_list, m)?)?;
    m.add_function(wrap_pyfunction!(send_tools_call, m)?)?;
    m.add_function(wrap_pyfunction!(send_resources_list, m)?)?;
    m.add_function(wrap_pyfunction!(send_resources_read, m)?)?;
    m.add_function(wrap_pyfunction!(send_prompts_list, m)?)?;
    m.add_function(wrap_pyfunction!(send_prompts_get, m)?)?;
    m.add_function(wrap_pyfunction!(send_ping, m)?)?;
    m.add_function(wrap_pyfunction!(send_message, m)?)?;
    m.add_function(wrap_pyfunction!(send_initialized_notification, m)?)?;
    m.add_function(wrap_pyfunction!(send_progress_notification, m)?)?;
    m.add_function(wrap_pyfunction!(send_cancelled_notification, m)?)?;
    m.add_function(wrap_pyfunction!(send_roots_list_changed_notification, m)?)?;
    m.add_function(wrap_pyfunction!(send_roots_list, m)?)?;
    m.add_function(wrap_pyfunction!(send_resources_subscribe, m)?)?;
    m.add_function(wrap_pyfunction!(send_resources_unsubscribe, m)?)?;
    Ok(())
}
