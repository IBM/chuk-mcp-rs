//! The server half of the protocol, for a Python dispatcher.
//!
//! `chuk-mcp-server` brings its own HTTP serving, routing and registries; what
//! it needs from here is the wire contract — what a `server/discover` result
//! looks like, how to tell a `2026-07-28` request from a legacy one, what to
//! reject an unspeakable version with, and how to say "I need more input".
//!
//! Everything here takes and returns plain dicts, so a Python server can use
//! it a message at a time without adopting a framework. The logic is the same
//! code the Rust server runs — the point is that there is one implementation
//! of the wire contract, not two.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{Map, Value};

use chuk_mcp::protocol::json_rpc::parse_message;
use chuk_mcp::protocol::mrtr::{
    input_required_result as build_input_required, InputRequests, RequestState,
};
use chuk_mcp::protocol::types::capabilities::ServerCapabilities;
use chuk_mcp::protocol::types::info::ServerInfo;
use chuk_mcp::server::{discover, modern};

use crate::{json_to_py, py_to_json, to_py_err};

/// Keys of the rejection a caller turns into a JSON-RPC error.
const KEY_CODE: &str = "code";
const KEY_MESSAGE: &str = "message";
const KEY_DATA: &str = "data";

/// The result of a `server/discover` request.
///
/// Shaped as the specification describes rather than as `initialize` was:
/// versions under `supportedVersions`, and identity in a reserved `_meta` key
/// rather than beside `capabilities`.
#[pyfunction]
#[pyo3(signature = (name, version, capabilities=None, instructions=None))]
fn discover_result(
    py: Python<'_>,
    name: &str,
    version: &str,
    capabilities: Option<Bound<'_, PyAny>>,
    instructions: Option<&str>,
) -> PyResult<Py<PyAny>> {
    let capabilities: ServerCapabilities = match capabilities {
        Some(value) => serde_json::from_value(py_to_json(&value)?)
            .map_err(|error| pyo3::exceptions::PyValueError::new_err(format!("{error}")))?,
        None => ServerCapabilities::default(),
    };
    let info = ServerInfo::new(name, version);

    json_to_py(
        py,
        &discover::discover_result(&info, &capabilities, instructions),
    )
}

/// Parse a message dict, or explain why it is not one.
fn message_of(
    message: &Bound<'_, PyAny>,
) -> PyResult<chuk_mcp::protocol::json_rpc::JsonRpcMessage> {
    parse_message(&py_to_json(message)?).map_err(to_py_err)
}

/// The protocol version a request declared, or `None` if it declared none.
///
/// Only the modern era declares per-request; a legacy client says it once, in
/// `initialize`.
#[pyfunction]
fn declared_version(message: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
    Ok(modern::declared_version(&message_of(message)?))
}

/// Whether a request is a `2026-07-28` one.
#[pyfunction]
fn is_modern_request(message: &Bound<'_, PyAny>) -> PyResult<bool> {
    Ok(modern::is_modern_request(&message_of(message)?))
}

/// Check a declared version.
///
/// `None` means it is fine. Otherwise a dict of `code`, `message` and `data`,
/// ready to become a JSON-RPC error — the `data` carries the versions this
/// build speaks, without which the client has nothing to renegotiate from.
#[pyfunction]
fn check_version(py: Python<'_>, message: &Bound<'_, PyAny>) -> PyResult<Option<Py<PyAny>>> {
    let Some((code, text, data)) = modern::reject_unsupported_version(&message_of(message)?) else {
        return Ok(None);
    };

    let rejection = PyDict::new(py);
    rejection.set_item(KEY_CODE, code)?;
    rejection.set_item(KEY_MESSAGE, text)?;
    rejection.set_item(KEY_DATA, json_to_py(py, &data.unwrap_or(Value::Null))?)?;
    Ok(Some(rejection.into_any().unbind()))
}

/// Stamp `resultType: "complete"` on a result that declares none.
///
/// For modern responses only: adding it to a legacy one would send a field
/// that revision never defined. A result that already declares its own — an
/// `input_required` — is returned untouched.
#[pyfunction]
fn stamp_result_type(py: Python<'_>, result: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let mut value = py_to_json(result)?;
    modern::stamp_result_type(&mut value);
    json_to_py(py, &value)
}

/// Build an `input_required` result — the server side of MRTR.
///
/// `input_requests` maps server-assigned keys to `{"method": ..., "params":
/// ...}` requests; `request_state` is an opaque string the client echoes back
/// verbatim on its retry. At least one of the two is required: neither would
/// ask the client to resend an identical request and expect a different
/// answer.
#[pyfunction]
#[pyo3(signature = (input_requests=None, request_state=None))]
fn input_required_result(
    py: Python<'_>,
    input_requests: Option<Bound<'_, PyAny>>,
    request_state: Option<String>,
) -> PyResult<Py<PyAny>> {
    let requests: InputRequests = match input_requests {
        Some(value) => serde_json::from_value(py_to_json(&value)?).map_err(|error| {
            pyo3::exceptions::PyValueError::new_err(format!("malformed inputRequests: {error}"))
        })?,
        None => InputRequests::new(),
    };

    let result =
        build_input_required(requests, request_state.map(RequestState::new)).map_err(to_py_err)?;
    json_to_py(py, &result)
}

/// One `elicitation/create` request, ready to go in `input_requests`.
///
/// A convenience over spelling the shape out: `mode` defaults to `"form"`, and
/// a form request carries `requested_schema` while a URL request carries `url`.
#[pyfunction]
#[pyo3(signature = (message, requested_schema=None, mode=None, url=None))]
fn elicit_request(
    py: Python<'_>,
    message: &str,
    requested_schema: Option<Bound<'_, PyAny>>,
    mode: Option<&str>,
    url: Option<&str>,
) -> PyResult<Py<PyAny>> {
    let mut params = Map::new();
    params.insert(
        "mode".to_string(),
        Value::String(mode.unwrap_or("form").to_string()),
    );
    params.insert("message".to_string(), Value::String(message.to_string()));
    if let Some(schema) = requested_schema {
        params.insert("requestedSchema".to_string(), py_to_json(&schema)?);
    }
    if let Some(url) = url {
        params.insert("url".to_string(), Value::String(url.to_string()));
    }

    let mut request = Map::new();
    request.insert(
        "method".to_string(),
        Value::String(
            chuk_mcp::protocol::messages::method::MessageMethod::ELICITATION_CREATE.to_string(),
        ),
    );
    request.insert("params".to_string(), Value::Object(params));

    json_to_py(py, &Value::Object(request))
}

/// Register the server-side protocol helpers on the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(discover_result, m)?)?;
    m.add_function(wrap_pyfunction!(declared_version, m)?)?;
    m.add_function(wrap_pyfunction!(is_modern_request, m)?)?;
    m.add_function(wrap_pyfunction!(check_version, m)?)?;
    m.add_function(wrap_pyfunction!(stamp_result_type, m)?)?;
    m.add_function(wrap_pyfunction!(input_required_result, m)?)?;
    m.add_function(wrap_pyfunction!(elicit_request, m)?)?;
    Ok(())
}
