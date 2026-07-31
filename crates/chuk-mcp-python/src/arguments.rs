//! Order-independent resolution of registration arguments.
//!
//! `chuk_mcp` has always taken the handler second — `register_tool(name,
//! handler, schema, description)` — while the Rust core takes it last, which is
//! what reads well for a closure and what the wider MCP ecosystem does. Neither
//! order is wrong, and changing either one breaks somebody.
//!
//! So both are accepted. The three values have disjoint types — a callable can
//! only be the handler, a mapping can only be the schema, a string can only be
//! the description — so resolving them by kind rather than by position guesses
//! at nothing, and an argument that fits none of the three is an error rather
//! than a silent misreading.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};

/// The pieces `register_tool` needs, whatever order they arrived in.
pub(crate) struct ToolParts<'py> {
    pub handler: Bound<'py, PyAny>,
    pub schema: Bound<'py, PyAny>,
    pub description: String,
}

impl<'py> ToolParts<'py> {
    /// Sort the three optional arguments by what they are.
    pub fn resolve(values: [Option<Bound<'py, PyAny>>; 3]) -> PyResult<ToolParts<'py>> {
        let mut handler = None;
        let mut schema = None;
        let mut description = None;

        for value in values.into_iter().flatten() {
            let slot = if value.is_callable() {
                &mut handler
            } else if value.is_instance_of::<PyDict>() {
                &mut schema
            } else if value.is_instance_of::<PyString>() {
                &mut description
            } else {
                return Err(PyTypeError::new_err(format!(
                    "register_tool takes a callable handler, a dict schema and a str \
                     description in any order; got {} which is none of those",
                    type_name(&value)
                )));
            };

            if slot.is_some() {
                return Err(PyTypeError::new_err(
                    "register_tool was given two arguments of the same kind; it takes one \
                     callable handler, one dict schema and one str description",
                ));
            }
            *slot = Some(value);
        }

        Ok(ToolParts {
            handler: handler
                .ok_or_else(|| PyTypeError::new_err("register_tool needs a callable handler"))?,
            schema: schema
                .ok_or_else(|| PyTypeError::new_err("register_tool needs a dict schema"))?,
            description: match description {
                Some(value) => value.extract()?,
                None => String::new(),
            },
        })
    }
}

/// The pieces `register_resource` needs.
///
/// Its other three arguments are all strings, so only the handler can move: it
/// is picked out wherever it appears, and the strings keep their order.
pub(crate) struct ResourceParts<'py> {
    pub handler: Bound<'py, PyAny>,
    pub name: String,
    pub description: String,
    pub mime_type: String,
}

impl<'py> ResourceParts<'py> {
    pub fn resolve(
        values: [Option<Bound<'py, PyAny>>; 4],
        default_mime_type: &str,
    ) -> PyResult<ResourceParts<'py>> {
        let mut handler = None;
        let mut strings: Vec<String> = Vec::new();

        for value in values.into_iter().flatten() {
            if value.is_callable() {
                if handler.is_some() {
                    return Err(PyTypeError::new_err(
                        "register_resource takes exactly one callable handler",
                    ));
                }
                handler = Some(value);
            } else if value.is_instance_of::<PyString>() {
                strings.push(value.extract()?);
            } else {
                return Err(PyTypeError::new_err(format!(
                    "register_resource takes a callable handler and str name, description \
                     and mime_type; got {} which is none of those",
                    type_name(&value)
                )));
            }
        }

        let mut strings = strings.into_iter();
        Ok(ResourceParts {
            handler: handler.ok_or_else(|| {
                PyTypeError::new_err("register_resource needs a callable handler")
            })?,
            name: strings.next().unwrap_or_default(),
            description: strings.next().unwrap_or_default(),
            mime_type: strings
                .next()
                .unwrap_or_else(|| default_mime_type.to_string()),
        })
    }
}

/// The Python type name of a value, for error messages that name what arrived
/// rather than only what was wanted.
fn type_name(value: &Bound<'_, PyAny>) -> String {
    value
        .get_type()
        .name()
        .map(|name| name.to_string())
        .unwrap_or_else(|_| "an unknown type".to_string())
}
