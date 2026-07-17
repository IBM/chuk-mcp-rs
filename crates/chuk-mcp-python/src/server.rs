//! Server-side `ProtocolHandler` bindings, mirroring the historical
//! `chuk_mcp.server` API (`server.protocol_handler.register_method(...)`,
//! `create_response(...)`, `create_error_response(...)`).

use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyTuple;
use serde_json::Value;
use tokio::sync::Mutex;

use chuk_mcp::protocol::json_rpc::{
    create_error_response, create_response, parse_message, JsonRpcMessage, RequestId,
};
use chuk_mcp::server::{method_handler, McpServer as CoreServer};

use crate::{coerce_to_json, json_to_py, py_to_json, to_py_err};

/// A JSON-RPC message passed to / returned from custom method handlers.
#[pyclass(name = "JSONRPCMessage")]
#[derive(Clone)]
pub struct PyJsonRpcMessage {
    pub(crate) inner: JsonRpcMessage,
}

#[pymethods]
impl PyJsonRpcMessage {
    #[getter]
    fn id(&self, py: Python<'_>) -> PyResult<PyObject> {
        match self.inner.id() {
            Some(id) => json_to_py(py, &serde_json::to_value(id).unwrap_or(Value::Null)),
            None => Ok(py.None()),
        }
    }
    #[getter]
    fn method(&self) -> Option<&str> {
        self.inner.method()
    }
    #[getter]
    fn params(&self, py: Python<'_>) -> PyResult<PyObject> {
        match self.inner.params() {
            Some(v) => json_to_py(py, v),
            None => Ok(py.None()),
        }
    }
    #[getter]
    fn result(&self, py: Python<'_>) -> PyResult<PyObject> {
        match self.inner.result() {
            Some(v) => json_to_py(py, v),
            None => Ok(py.None()),
        }
    }
    #[getter]
    fn error(&self, py: Python<'_>) -> PyResult<PyObject> {
        match self.inner.error() {
            Some(e) => json_to_py(py, &serde_json::to_value(e).unwrap_or(Value::Null)),
            None => Ok(py.None()),
        }
    }

    #[pyo3(signature = (**_kwargs))]
    fn model_dump(
        &self,
        py: Python<'_>,
        _kwargs: Option<Bound<'_, pyo3::types::PyDict>>,
    ) -> PyResult<PyObject> {
        json_to_py(py, &self.inner.to_value())
    }

    fn __repr__(&self) -> String {
        format!(
            "JSONRPCMessage(method={:?}, id={:?})",
            self.inner.method(),
            self.inner.id()
        )
    }
}

fn extract_request_id(value: &Bound<'_, PyAny>) -> PyResult<RequestId> {
    if let Ok(n) = value.extract::<i64>() {
        Ok(RequestId::Num(n))
    } else if let Ok(s) = value.extract::<String>() {
        Ok(RequestId::Str(s))
    } else {
        Err(PyValueError::new_err("message id must be an int or str"))
    }
}

/// Server-side protocol handler: register custom method handlers and build
/// responses. Obtained via `server.protocol_handler`.
#[pyclass(name = "ProtocolHandler")]
pub struct PyProtocolHandler {
    pub(crate) server: Arc<Mutex<Option<CoreServer>>>,
}

#[pymethods]
impl PyProtocolHandler {
    /// Register an async handler `handler(message, session_id) -> (response, session)`
    /// for `method`. `response` is a JSONRPCMessage (from `create_response` /
    /// `create_error_response`) or None; `session` is usually None.
    fn register_method(&self, method: String, handler: PyObject) -> PyResult<()> {
        let handler = Arc::new(handler);
        let bridge = method_handler(move |msg, session| {
            let handler = handler.clone();
            async move {
                // Call the Python handler with a message object + session id.
                let future = Python::with_gil(|py| -> PyResult<_> {
                    let msg_obj = Py::new(py, PyJsonRpcMessage { inner: msg })?;
                    let coroutine = handler.bind(py).call1((msg_obj, session.clone()))?;
                    pyo3_async_runtimes::tokio::into_future(coroutine)
                })
                .map_err(|e| e.to_string())?;

                let result = future.await.map_err(|e| e.to_string())?;

                Python::with_gil(|py| {
                    let bound = result.bind(py);
                    // Accept either (response, session) or a bare response.
                    let (resp_item, session_item) = match bound.downcast::<PyTuple>() {
                        Ok(tuple) => {
                            let resp = tuple.get_item(0).map_err(|e| e.to_string())?;
                            let sess = if tuple.len() > 1 {
                                tuple.get_item(1).ok()
                            } else {
                                None
                            };
                            (resp, sess)
                        }
                        Err(_) => (bound.clone(), None),
                    };

                    let response = if resp_item.is_none() {
                        None
                    } else {
                        let pymsg: PyRef<PyJsonRpcMessage> =
                            resp_item.extract().map_err(|e: PyErr| e.to_string())?;
                        Some(pymsg.inner.clone())
                    };

                    let new_session = session_item
                        .filter(|s| !s.is_none())
                        .and_then(|s| s.extract::<String>().ok());

                    Ok((response, new_session))
                })
            }
        });

        let mut guard = self
            .server
            .try_lock()
            .map_err(|_| PyRuntimeError::new_err("Server is busy"))?;
        let server = guard
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Server already running"))?;
        server.protocol_handler.register_method(&method, bridge);
        Ok(())
    }

    /// Dispatch a message through the full server (built-in tool/resource
    /// handling + custom method handlers). Returns `(response, session)` where
    /// `response` is a JSONRPCMessage or None. `message` may be a JSONRPCMessage
    /// model, a dict, or this module's message object.
    #[pyo3(signature = (message, session_id=None))]
    fn handle_message<'py>(
        &self,
        py: Python<'py>,
        message: Bound<'py, PyAny>,
        session_id: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let value = coerce_to_json(&message)?;
        let msg = parse_message(&value).map_err(to_py_err)?;
        let server = self.server.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = server.lock().await;
            let srv = guard
                .as_ref()
                .ok_or_else(|| PyRuntimeError::new_err("Server not available"))?;
            let (response, new_session) = srv.handle_message(msg, session_id.as_deref()).await;
            Python::with_gil(|py| -> PyResult<PyObject> {
                let resp: PyObject = match response {
                    Some(inner) => Py::new(py, PyJsonRpcMessage { inner })?.into_any(),
                    None => py.None(),
                };
                Ok((resp, new_session).into_pyobject(py)?.into_any().unbind())
            })
        })
    }

    /// Build a success response for `msg_id` carrying `result`.
    fn create_response(
        &self,
        msg_id: Bound<'_, PyAny>,
        result: Bound<'_, PyAny>,
    ) -> PyResult<PyJsonRpcMessage> {
        let id = extract_request_id(&msg_id)?;
        let result = py_to_json(&result)?;
        Ok(PyJsonRpcMessage {
            inner: JsonRpcMessage::Response(create_response(id, Some(result))),
        })
    }

    /// Build an error response. The second argument may be either an error code
    /// (with an optional message) or an error dict `{"code": ..., "message": ...}`.
    #[pyo3(signature = (msg_id, code, message=None))]
    fn create_error_response(
        &self,
        msg_id: Bound<'_, PyAny>,
        code: Bound<'_, PyAny>,
        message: Option<String>,
    ) -> PyResult<PyJsonRpcMessage> {
        let id = extract_request_id(&msg_id)?;

        let (code, message, data) = if let Ok(dict) = py_to_json(&code) {
            if let Value::Object(map) = &dict {
                let code = map.get("code").and_then(Value::as_i64).unwrap_or(-32603);
                let msg = map
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or(message)
                    .unwrap_or_else(|| "error".to_string());
                let data = map.get("data").cloned();
                (code, msg, data)
            } else {
                let code = dict.as_i64().unwrap_or(-32603);
                (code, message.unwrap_or_else(|| "error".to_string()), None)
            }
        } else {
            let code = code.extract::<i64>().unwrap_or(-32603);
            (code, message.unwrap_or_else(|| "error".to_string()), None)
        };

        Ok(PyJsonRpcMessage {
            inner: JsonRpcMessage::Error(create_error_response(id, code, &message, data)),
        })
    }
}

/// Register the server-side classes on the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyJsonRpcMessage>()?;
    m.add_class::<PyProtocolHandler>()?;
    Ok(())
}
