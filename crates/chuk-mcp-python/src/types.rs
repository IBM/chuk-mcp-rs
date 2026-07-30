//! Typed Python wrappers around the core result types, giving attribute access
//! that mirrors the `chuk_mcp` Pydantic models (`tool.name`, `result.content`,
//! `resource.mimeType`, …). Each wrapper also offers `to_dict()` for the raw
//! JSON form.

use pyo3::prelude::*;
use pythonize::pythonize;
use serde::Serialize;
use serde_json::Value;

use chuk_mcp::protocol::messages::initialize::InitializeResult;
use chuk_mcp::protocol::messages::prompts::{
    GetPromptResult, ListPromptsResult, Prompt, PromptArgument, PromptMessage,
};
use chuk_mcp::protocol::messages::resources::{
    ListResourcesResult, ReadResourceResult, Resource, ResourceContent,
};
use chuk_mcp::protocol::messages::tools::{ListToolsResult, Tool, ToolResult};
use chuk_mcp::protocol::types::capabilities::ServerCapabilities as CoreServerCapabilities;
use chuk_mcp::protocol::types::info::ServerInfo;
use serde_json::Map;

fn json_to_py(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    Ok(pythonize(py, value)?.unbind())
}

fn to_dict<T: Serialize>(py: Python<'_>, value: &T) -> PyResult<Py<PyAny>> {
    let json = serde_json::to_value(value)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("serialize error: {e}")))?;
    json_to_py(py, &json)
}

/// One capability section (e.g. the value of `capabilities.tools`).
///
/// Present sections are always truthy (matching the Pydantic models, where an
/// empty `ToolsCapability()` object is still truthy), and expose their fields
/// both by attribute (`caps.tools.listChanged`) and item (`caps.tools["listChanged"]`).
#[pyclass(name = "CapabilitySection", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyCapabilitySection {
    data: Map<String, Value>,
}

#[pymethods]
impl PyCapabilitySection {
    fn __bool__(&self) -> bool {
        true
    }

    fn __getattr__(&self, py: Python<'_>, name: &str) -> PyResult<Py<PyAny>> {
        // Don't intercept dunder lookups.
        if name.starts_with("__") && name.ends_with("__") {
            return Err(pyo3::exceptions::PyAttributeError::new_err(
                name.to_string(),
            ));
        }
        match self.data.get(name) {
            Some(v) => json_to_py(py, v),
            None => Ok(py.None()),
        }
    }

    fn __getitem__(&self, py: Python<'_>, key: &str) -> PyResult<Py<PyAny>> {
        match self.data.get(key) {
            Some(v) => json_to_py(py, v),
            None => Err(pyo3::exceptions::PyKeyError::new_err(key.to_string())),
        }
    }

    #[pyo3(signature = (key, default=None))]
    fn get(&self, py: Python<'_>, key: &str, default: Option<Py<PyAny>>) -> PyResult<Py<PyAny>> {
        match self.data.get(key) {
            Some(v) => json_to_py(py, v),
            None => Ok(default.unwrap_or_else(|| py.None())),
        }
    }

    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_to_py(py, &Value::Object(self.data.clone()))
    }

    fn __repr__(&self) -> String {
        format!(
            "CapabilitySection({:?})",
            self.data.keys().collect::<Vec<_>>()
        )
    }
}

fn to_section<T: Serialize>(opt: &Option<T>) -> Option<PyCapabilitySection> {
    opt.as_ref().map(|v| {
        let data = serde_json::to_value(v)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        PyCapabilitySection { data }
    })
}

/// Capabilities a server supports. Sections are `None` when absent and a
/// (truthy) [`PyCapabilitySection`] when present.
#[pyclass(name = "ServerCapabilities", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyServerCapabilities {
    pub(crate) inner: CoreServerCapabilities,
}

#[pymethods]
impl PyServerCapabilities {
    #[getter]
    fn tools(&self) -> Option<PyCapabilitySection> {
        to_section(&self.inner.tools)
    }
    #[getter]
    fn resources(&self) -> Option<PyCapabilitySection> {
        to_section(&self.inner.resources)
    }
    #[getter]
    fn prompts(&self) -> Option<PyCapabilitySection> {
        to_section(&self.inner.prompts)
    }
    #[getter]
    fn logging(&self) -> Option<PyCapabilitySection> {
        to_section(&self.inner.logging)
    }
    #[getter]
    fn completion(&self) -> Option<PyCapabilitySection> {
        to_section(&self.inner.completion)
    }
    #[getter]
    fn experimental(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        match &self.inner.experimental {
            Some(m) => to_dict(py, m),
            None => Ok(py.None()),
        }
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        let mut present = Vec::new();
        if self.inner.tools.is_some() {
            present.push("tools");
        }
        if self.inner.resources.is_some() {
            present.push("resources");
        }
        if self.inner.prompts.is_some() {
            present.push("prompts");
        }
        if self.inner.logging.is_some() {
            present.push("logging");
        }
        if self.inner.completion.is_some() {
            present.push("completion");
        }
        format!("ServerCapabilities({})", present.join(", "))
    }
}

impl From<CoreServerCapabilities> for PyServerCapabilities {
    fn from(inner: CoreServerCapabilities) -> Self {
        PyServerCapabilities { inner }
    }
}

/// Information about the server implementation.
#[pyclass(name = "ServerInfo", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyServerInfo {
    pub(crate) inner: ServerInfo,
}

#[pymethods]
impl PyServerInfo {
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }
    #[getter]
    fn version(&self) -> &str {
        &self.inner.version
    }
    #[getter]
    fn title(&self) -> Option<&str> {
        self.inner.title.as_deref()
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!(
            "ServerInfo(name={:?}, version={:?})",
            self.inner.name, self.inner.version
        )
    }
}

impl From<ServerInfo> for PyServerInfo {
    fn from(inner: ServerInfo) -> Self {
        PyServerInfo { inner }
    }
}

/// A tool definition.
#[pyclass(name = "Tool", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyTool {
    pub(crate) inner: Tool,
}

#[pymethods]
impl PyTool {
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }
    #[getter]
    fn description(&self) -> Option<&str> {
        self.inner.description.as_deref()
    }
    /// The JSON Schema for the tool's input (matches the `inputSchema` field).
    #[getter(inputSchema)]
    fn input_schema(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_to_py(py, &self.inner.input_schema)
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!("Tool(name={:?})", self.inner.name)
    }
}

impl From<Tool> for PyTool {
    fn from(inner: Tool) -> Self {
        PyTool { inner }
    }
}

/// Result of a `tools/call`.
#[pyclass(name = "ToolResult", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyToolResult {
    pub(crate) inner: ToolResult,
}

#[pymethods]
impl PyToolResult {
    /// Content blocks returned by the tool (list of dicts).
    #[getter]
    fn content(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_to_py(py, &Value::Array(self.inner.content.clone()))
    }
    #[getter(isError)]
    fn is_error(&self) -> bool {
        self.inner.is_error
    }
    /// Concatenated text of all text content blocks.
    #[getter]
    fn text(&self) -> String {
        self.inner.text()
    }
    /// The result envelope's `resultType`. A legacy result normalises to
    /// `"complete"`, so this is always set whichever era produced the result.
    #[getter(resultType)]
    fn result_type(&self) -> &str {
        &self.inner.result_type
    }
    /// The flattened 0.9-era value: structured data if the tool returned any,
    /// else the text, else the raw content blocks.
    #[getter]
    fn value(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_to_py(py, &self.inner.value())
    }
    /// Structured content blocks, if any (list of dicts, or None).
    #[getter(structuredContent)]
    fn structured_content(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match self.inner.structured_content() {
            Some(blocks) => Ok(Some(json_to_py(py, &Value::Array(blocks.clone()))?)),
            None => Ok(None),
        }
    }
    /// The server's self-reported identity from `_meta` (a dict, or None).
    /// Unverified — for display and attribution only.
    #[getter(serverIdentity)]
    fn server_identity(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match self.inner.server_identity() {
            Some(id) => Ok(Some(json_to_py(py, &id)?)),
            None => Ok(None),
        }
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!(
            "ToolResult(blocks={}, resultType={:?}, isError={})",
            self.inner.content.len(),
            self.inner.result_type,
            self.inner.is_error
        )
    }
}

impl From<ToolResult> for PyToolResult {
    fn from(inner: ToolResult) -> Self {
        PyToolResult { inner }
    }
}

/// A resource definition.
#[pyclass(name = "Resource", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyResource {
    pub(crate) inner: Resource,
}

#[pymethods]
impl PyResource {
    #[getter]
    fn uri(&self) -> &str {
        &self.inner.uri
    }
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }
    #[getter]
    fn description(&self) -> Option<&str> {
        self.inner.description.as_deref()
    }
    #[getter(mimeType)]
    fn mime_type(&self) -> Option<&str> {
        self.inner.mime_type.as_deref()
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!(
            "Resource(uri={:?}, name={:?})",
            self.inner.uri, self.inner.name
        )
    }
}

impl From<Resource> for PyResource {
    fn from(inner: Resource) -> Self {
        PyResource { inner }
    }
}

/// Contents of a read resource (text or base64 blob).
#[pyclass(name = "ResourceContent", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyResourceContent {
    pub(crate) inner: ResourceContent,
}

#[pymethods]
impl PyResourceContent {
    #[getter]
    fn uri(&self) -> &str {
        &self.inner.uri
    }
    #[getter(mimeType)]
    fn mime_type(&self) -> Option<&str> {
        self.inner.mime_type.as_deref()
    }
    #[getter]
    fn text(&self) -> Option<&str> {
        self.inner.text.as_deref()
    }
    #[getter]
    fn blob(&self) -> Option<&str> {
        self.inner.blob.as_deref()
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!("ResourceContent(uri={:?})", self.inner.uri)
    }
}

impl From<ResourceContent> for PyResourceContent {
    fn from(inner: ResourceContent) -> Self {
        PyResourceContent { inner }
    }
}

/// Result of a `resources/read`.
#[pyclass(name = "ReadResourceResult", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyReadResourceResult {
    pub(crate) inner: ReadResourceResult,
}

#[pymethods]
impl PyReadResourceResult {
    #[getter]
    fn contents(&self) -> Vec<PyResourceContent> {
        self.inner
            .contents
            .iter()
            .cloned()
            .map(PyResourceContent::from)
            .collect()
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!("ReadResourceResult(contents={})", self.inner.contents.len())
    }
}

impl From<ReadResourceResult> for PyReadResourceResult {
    fn from(inner: ReadResourceResult) -> Self {
        PyReadResourceResult { inner }
    }
}

/// An argument a prompt template accepts.
#[pyclass(name = "PromptArgument", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyPromptArgument {
    pub(crate) inner: PromptArgument,
}

#[pymethods]
impl PyPromptArgument {
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }
    #[getter]
    fn description(&self) -> Option<&str> {
        self.inner.description.as_deref()
    }
    #[getter]
    fn required(&self) -> Option<bool> {
        self.inner.required
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!("PromptArgument(name={:?})", self.inner.name)
    }
}

impl From<PromptArgument> for PyPromptArgument {
    fn from(inner: PromptArgument) -> Self {
        PyPromptArgument { inner }
    }
}

/// A prompt definition.
#[pyclass(name = "Prompt", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyPrompt {
    pub(crate) inner: Prompt,
}

#[pymethods]
impl PyPrompt {
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }
    #[getter]
    fn description(&self) -> Option<&str> {
        self.inner.description.as_deref()
    }
    #[getter]
    fn arguments(&self) -> Option<Vec<PyPromptArgument>> {
        self.inner
            .arguments
            .as_ref()
            .map(|args| args.iter().cloned().map(PyPromptArgument::from).collect())
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!("Prompt(name={:?})", self.inner.name)
    }
}

impl From<Prompt> for PyPrompt {
    fn from(inner: Prompt) -> Self {
        PyPrompt { inner }
    }
}

/// A message within a prompt.
#[pyclass(name = "PromptMessage", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyPromptMessage {
    pub(crate) inner: PromptMessage,
}

#[pymethods]
impl PyPromptMessage {
    #[getter]
    fn role(&self) -> &str {
        &self.inner.role
    }
    #[getter]
    fn content(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_to_py(py, &self.inner.content)
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!("PromptMessage(role={:?})", self.inner.role)
    }
}

impl From<PromptMessage> for PyPromptMessage {
    fn from(inner: PromptMessage) -> Self {
        PyPromptMessage { inner }
    }
}

/// Result of a `prompts/get`.
#[pyclass(name = "GetPromptResult", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyGetPromptResult {
    pub(crate) inner: GetPromptResult,
}

#[pymethods]
impl PyGetPromptResult {
    #[getter]
    fn description(&self) -> Option<&str> {
        self.inner.description.as_deref()
    }
    #[getter]
    fn messages(&self) -> Option<Vec<PyPromptMessage>> {
        self.inner
            .messages
            .as_ref()
            .map(|msgs| msgs.iter().cloned().map(PyPromptMessage::from).collect())
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        let n = self.inner.messages.as_ref().map(Vec::len).unwrap_or(0);
        format!("GetPromptResult(messages={n})")
    }
}

impl From<GetPromptResult> for PyGetPromptResult {
    fn from(inner: GetPromptResult) -> Self {
        PyGetPromptResult { inner }
    }
}

/// Result of `initialize`.
#[pyclass(name = "InitializeResult", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyInitializeResult {
    pub(crate) inner: InitializeResult,
}

#[pymethods]
impl PyInitializeResult {
    #[getter(protocolVersion)]
    fn protocol_version(&self) -> &str {
        &self.inner.protocol_version
    }
    #[getter(serverInfo)]
    fn server_info(&self) -> PyServerInfo {
        PyServerInfo::from(self.inner.server_info.clone())
    }
    #[getter]
    fn capabilities(&self) -> PyServerCapabilities {
        PyServerCapabilities::from(self.inner.capabilities.clone())
    }
    #[getter]
    fn instructions(&self) -> Option<&str> {
        self.inner.instructions.as_deref()
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!(
            "InitializeResult(server={:?}, protocol={:?})",
            self.inner.server_info.name, self.inner.protocol_version
        )
    }
}

impl From<InitializeResult> for PyInitializeResult {
    fn from(inner: InitializeResult) -> Self {
        PyInitializeResult { inner }
    }
}

/// Result of `tools/list`.
#[pyclass(name = "ListToolsResult", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyListToolsResult {
    pub(crate) inner: ListToolsResult,
}

#[pymethods]
impl PyListToolsResult {
    #[getter]
    fn tools(&self) -> Vec<PyTool> {
        self.inner.tools.iter().cloned().map(PyTool::from).collect()
    }
    #[getter(nextCursor)]
    fn next_cursor(&self) -> Option<&str> {
        self.inner.next_cursor.as_deref()
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!("ListToolsResult(tools={})", self.inner.tools.len())
    }
}

impl From<ListToolsResult> for PyListToolsResult {
    fn from(inner: ListToolsResult) -> Self {
        PyListToolsResult { inner }
    }
}

/// Result of `resources/list`.
#[pyclass(name = "ListResourcesResult", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyListResourcesResult {
    pub(crate) inner: ListResourcesResult,
}

#[pymethods]
impl PyListResourcesResult {
    #[getter]
    fn resources(&self) -> Vec<PyResource> {
        self.inner
            .resources
            .iter()
            .cloned()
            .map(PyResource::from)
            .collect()
    }
    #[getter(nextCursor)]
    fn next_cursor(&self) -> Option<&str> {
        self.inner.next_cursor.as_deref()
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!(
            "ListResourcesResult(resources={})",
            self.inner.resources.len()
        )
    }
}

impl From<ListResourcesResult> for PyListResourcesResult {
    fn from(inner: ListResourcesResult) -> Self {
        PyListResourcesResult { inner }
    }
}

/// Result of `prompts/list`.
#[pyclass(name = "ListPromptsResult", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyListPromptsResult {
    pub(crate) inner: ListPromptsResult,
}

#[pymethods]
impl PyListPromptsResult {
    #[getter]
    fn prompts(&self) -> Vec<PyPrompt> {
        self.inner
            .prompts
            .iter()
            .cloned()
            .map(PyPrompt::from)
            .collect()
    }
    #[getter(nextCursor)]
    fn next_cursor(&self) -> Option<&str> {
        self.inner.next_cursor.as_deref()
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!("ListPromptsResult(prompts={})", self.inner.prompts.len())
    }
}

impl From<ListPromptsResult> for PyListPromptsResult {
    fn from(inner: ListPromptsResult) -> Self {
        PyListPromptsResult { inner }
    }
}

/// Register all typed classes on the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCapabilitySection>()?;
    m.add_class::<PyServerCapabilities>()?;
    m.add_class::<PyServerInfo>()?;
    m.add_class::<PyTool>()?;
    m.add_class::<PyToolResult>()?;
    m.add_class::<PyResource>()?;
    m.add_class::<PyResourceContent>()?;
    m.add_class::<PyReadResourceResult>()?;
    m.add_class::<PyPromptArgument>()?;
    m.add_class::<PyPrompt>()?;
    m.add_class::<PyPromptMessage>()?;
    m.add_class::<PyGetPromptResult>()?;
    m.add_class::<PyInitializeResult>()?;
    m.add_class::<PyListToolsResult>()?;
    m.add_class::<PyListResourcesResult>()?;
    m.add_class::<PyListPromptsResult>()?;
    Ok(())
}
