//! Remembering tool schemas so `tools/call` can promote its parameters.
//!
//! A tool may mark a parameter `x-mcp-header`, asking the client to mirror its
//! value into an `Mcp-Param-{Name}` header so a gateway can route on it without
//! parsing the body. Clients **MUST** support that.
//!
//! The awkwardness is that the instruction and the opportunity arrive at
//! different times: the annotation lives in the tool's `inputSchema`, which the
//! client saw in a `tools/list` result, while the promotion has to happen when
//! a later `tools/call` is turned into a request. Something has to carry the
//! schema across that gap, and this is it — a small cache that the transport
//! fills as it observes listings and reads as it builds envelopes.
//!
//! It is deliberately *not* a source of truth about what tools exist. It
//! remembers only what it has been told, forgets nothing, and a `tools/call`
//! for a name it has never seen listed is sent unpromoted rather than refused:
//! a caller who knows a tool's name without having listed it is doing something
//! legitimate, and no annotation can be applied to a schema nobody has.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::protocol::messages::method::MessageMethod;

/// Field names read out of a `tools/list` result.
const FIELD_TOOLS: &str = "tools";
const FIELD_NAME: &str = "name";
const FIELD_INPUT_SCHEMA: &str = "inputSchema";

/// The `inputSchema` of every tool this connection has seen listed.
///
/// Shared by clone: the transport hands copies to the tasks that dispatch
/// individual requests, and they all read and write the same map.
#[derive(Clone, Default)]
pub struct ToolSchemas {
    known: Arc<Mutex<BTreeMap<String, Value>>>,
}

impl ToolSchemas {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the schemas in a `tools/list` result.
    ///
    /// A malformed entry is skipped rather than failing the batch: one tool
    /// with no name should not cost the rest of the listing its promotion.
    pub fn record_listing(&self, result: &Value) {
        let Some(tools) = result.get(FIELD_TOOLS).and_then(Value::as_array) else {
            return;
        };
        let mut known = self.known.lock().expect("tool schema lock");
        for tool in tools {
            let (Some(name), Some(schema)) = (
                tool.get(FIELD_NAME).and_then(Value::as_str),
                tool.get(FIELD_INPUT_SCHEMA),
            ) else {
                continue;
            };
            known.insert(name.to_string(), schema.clone());
        }
    }

    /// Record from a response message, if it is one that carries a listing.
    ///
    /// `method` is the method of the request being answered — a response says
    /// nothing about what was asked, so the caller supplies it.
    pub fn observe(&self, method: &str, message: &crate::protocol::json_rpc::JsonRpcMessage) {
        if method != MessageMethod::TOOLS_LIST {
            return;
        }
        if let crate::protocol::json_rpc::JsonRpcMessage::Response(response) = message {
            self.record_listing(&response.result);
        }
    }

    /// The remembered schema for a tool, if it has been listed.
    pub fn get(&self, name: &str) -> Option<Value> {
        self.known
            .lock()
            .expect("tool schema lock")
            .get(name)
            .cloned()
    }

    /// How many schemas are remembered.
    pub fn len(&self) -> usize {
        self.known.lock().expect("tool schema lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::json_rpc::{create_response, JsonRpcMessage, RequestId};
    use serde_json::json;

    fn listing() -> Value {
        json!({
            "tools": [
                {
                    "name": "get_weather",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"region": {"type": "string", "x-mcp-header": "Region"}},
                    },
                },
                {"name": "plain", "inputSchema": {"type": "object"}},
            ]
        })
    }

    #[test]
    fn a_listing_is_remembered_by_tool_name() {
        let schemas = ToolSchemas::new();
        schemas.record_listing(&listing());

        assert_eq!(schemas.len(), 2);
        let schema = schemas.get("get_weather").expect("a remembered schema");
        assert_eq!(
            schema["properties"]["region"]["x-mcp-header"],
            json!("Region")
        );
    }

    #[test]
    fn a_tool_never_listed_is_simply_unknown() {
        let schemas = ToolSchemas::new();
        schemas.record_listing(&listing());
        assert!(schemas.get("never-seen").is_none());
    }

    /// One malformed entry must not cost the others their promotion.
    #[test]
    fn an_entry_missing_a_name_or_schema_is_skipped() {
        let schemas = ToolSchemas::new();
        schemas.record_listing(&json!({
            "tools": [
                {"inputSchema": {"type": "object"}},
                {"name": "no-schema"},
                {"name": "good", "inputSchema": {"type": "object"}},
            ]
        }));
        assert_eq!(schemas.len(), 1);
        assert!(schemas.get("good").is_some());
    }

    #[test]
    fn a_result_that_is_not_a_listing_records_nothing() {
        let schemas = ToolSchemas::new();
        schemas.record_listing(&json!({"content": []}));
        assert!(schemas.is_empty());
    }

    /// A later listing wins: a server may change a tool's schema, and the
    /// promotion must follow the definition currently in force.
    #[test]
    fn a_later_listing_replaces_an_earlier_schema() {
        let schemas = ToolSchemas::new();
        schemas.record_listing(&json!({
            "tools": [{"name": "t", "inputSchema": {"version": 1}}]
        }));
        schemas.record_listing(&json!({
            "tools": [{"name": "t", "inputSchema": {"version": 2}}]
        }));
        assert_eq!(schemas.get("t").unwrap()["version"], json!(2));
    }

    #[test]
    fn only_a_tools_list_response_is_observed() {
        let schemas = ToolSchemas::new();
        let response =
            JsonRpcMessage::Response(create_response(RequestId::Num(1), Some(listing())));

        // A response to something else carrying a `tools` field by coincidence
        // must not be mistaken for a listing.
        schemas.observe(MessageMethod::TOOLS_CALL, &response);
        assert!(schemas.is_empty());

        schemas.observe(MessageMethod::TOOLS_LIST, &response);
        assert_eq!(schemas.len(), 2);
    }
}
