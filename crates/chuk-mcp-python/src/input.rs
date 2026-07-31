//! Answering server requests for input, from Python.
//!
//! The core answers elicitation through an [`InputHandler`]; this adapts a
//! Python async callable onto it, so a Python caller writes one coroutine and
//! it serves both protocol eras exactly as the Rust one does.

use std::sync::Arc;

use async_trait::async_trait;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::Value;

use chuk_mcp::client::input::InputHandler;
use chuk_mcp::protocol::mrtr::{ElicitRequest, ElicitResult};

use crate::{json_to_py, py_to_json};

/// The three answers a user can give, exposed so Python need not spell them.
pub(crate) const ACTION_ACCEPT: &str = "accept";
pub(crate) const ACTION_DECLINE: &str = "decline";
pub(crate) const ACTION_CANCEL: &str = "cancel";

/// Keys of the answer dict a handler returns.
const KEY_ACTION: &str = "action";
const KEY_CONTENT: &str = "content";

/// What a server is asking the user, as a Python object.
///
/// A class rather than a bare dict so a handler can read `request.message`
/// and `request.requestedSchema` without knowing the wire spelling of either,
/// while `to_dict()` remains available for anything not modelled here.
#[pyclass(name = "ElicitRequest", skip_from_py_object)]
#[derive(Clone)]
pub(crate) struct PyElicitRequest {
    inner: ElicitRequest,
}

#[pymethods]
impl PyElicitRequest {
    /// `"form"` or `"url"`. A request that omitted it is a form request.
    #[getter]
    fn mode(&self) -> &'static str {
        match self.inner.mode {
            chuk_mcp::protocol::mrtr::ElicitMode::Form => "form",
            chuk_mcp::protocol::mrtr::ElicitMode::Url => "url",
        }
    }

    /// Why the input is needed, for display to the user.
    #[getter]
    fn message(&self) -> &str {
        &self.inner.message
    }

    /// Form mode: the JSON Schema the answer must match.
    #[getter(requestedSchema)]
    fn requested_schema(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match &self.inner.requested_schema {
            Some(schema) => Ok(Some(json_to_py(py, schema)?)),
            None => Ok(None),
        }
    }

    /// URL mode: where the user should be sent.
    #[getter]
    fn url(&self) -> Option<&str> {
        self.inner.url.as_deref()
    }

    /// The whole request as a dict.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let value = serde_json::to_value(&self.inner)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        json_to_py(py, &value)
    }

    fn __repr__(&self) -> String {
        format!(
            "ElicitRequest(mode={:?}, message={:?})",
            self.mode(),
            self.inner.message
        )
    }
}

/// Helpers for building the answer, so a handler need not remember the shape.
///
/// Returning a plain dict works too; these exist so the common cases cannot be
/// misspelled.
#[pyclass(name = "ElicitResult")]
pub(crate) struct PyElicitResult;

#[pymethods]
impl PyElicitResult {
    /// The user submitted `content`.
    #[staticmethod]
    #[pyo3(signature = (content=None))]
    fn accept(py: Python<'_>, content: Option<Py<PyAny>>) -> PyResult<Py<PyAny>> {
        let answer = PyDict::new(py);
        answer.set_item(KEY_ACTION, ACTION_ACCEPT)?;
        if let Some(content) = content {
            answer.set_item(KEY_CONTENT, content)?;
        }
        Ok(answer.into_any().unbind())
    }

    /// The user said no.
    #[staticmethod]
    fn decline(py: Python<'_>) -> PyResult<Py<PyAny>> {
        let answer = PyDict::new(py);
        answer.set_item(KEY_ACTION, ACTION_DECLINE)?;
        Ok(answer.into_any().unbind())
    }

    /// The user dismissed the request without choosing.
    #[staticmethod]
    fn cancel(py: Python<'_>) -> PyResult<Py<PyAny>> {
        let answer = PyDict::new(py);
        answer.set_item(KEY_ACTION, ACTION_CANCEL)?;
        Ok(answer.into_any().unbind())
    }
}

/// An [`InputHandler`] backed by a Python async callable.
pub(crate) struct PythonInputHandler {
    /// `async def handler(request: ElicitRequest) -> dict`
    callback: Py<PyAny>,
    url_mode: bool,
}

impl PythonInputHandler {
    /// Wrap a Python coroutine function as a handler the core can drive.
    pub(crate) fn into_handler(callback: Py<PyAny>, url_mode: bool) -> Arc<dyn InputHandler> {
        Arc::new(PythonInputHandler { callback, url_mode })
    }
}

#[async_trait]
impl InputHandler for PythonInputHandler {
    async fn elicit(&self, request: ElicitRequest) -> ElicitResult {
        match self.call_python(request).await {
            Ok(result) => result,
            // A handler that raises has not refused on the user's behalf — it
            // failed. `cancel` is the honest report of that: dismissed without
            // an explicit choice. Reporting `decline` would tell the server the
            // user said no, which nobody did.
            Err(error) => {
                tracing::warn!("the Python elicitation handler failed: {error}");
                ElicitResult::cancel()
            }
        }
    }

    fn supports_url_mode(&self) -> bool {
        self.url_mode
    }
}

impl PythonInputHandler {
    async fn call_python(&self, request: ElicitRequest) -> PyResult<ElicitResult> {
        // Build the argument and start the coroutine while attached, then drop
        // the GIL before awaiting it — holding it across an await would stall
        // every other Python thread for as long as the user takes to answer.
        let future = Python::attach(|py| -> PyResult<_> {
            let argument = PyElicitRequest { inner: request }.into_pyobject(py)?;
            let coroutine = self.callback.bind(py).call1((argument,))?;
            pyo3_async_runtimes::tokio::into_future(coroutine)
        })?;

        let answered = future.await?;
        Python::attach(|py| to_elicit_result(&answered.bind(py).clone()))
    }
}

/// Read whatever the handler returned as an [`ElicitResult`].
///
/// Accepts a dict, anything dict-like enough to convert, or `None` — which is
/// read as a cancellation rather than an error, since a coroutine that falls
/// off its end returning nothing has not decided anything.
fn to_elicit_result(returned: &Bound<'_, PyAny>) -> PyResult<ElicitResult> {
    if returned.is_none() {
        return Ok(ElicitResult::cancel());
    }

    let value: Value = py_to_json(returned)?;
    serde_json::from_value(value).map_err(|error| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "an elicitation handler must return {{'{KEY_ACTION}': \
             '{ACTION_ACCEPT}'|'{ACTION_DECLINE}'|'{ACTION_CANCEL}', \
             '{KEY_CONTENT}': {{...}}}} or None; {error}"
        ))
    })
}

/// Register the elicitation types on the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyElicitRequest>()?;
    m.add_class::<PyElicitResult>()?;
    m.add("ELICIT_ACCEPT", ACTION_ACCEPT)?;
    m.add("ELICIT_DECLINE", ACTION_DECLINE)?;
    m.add("ELICIT_CANCEL", ACTION_CANCEL)?;
    Ok(())
}
