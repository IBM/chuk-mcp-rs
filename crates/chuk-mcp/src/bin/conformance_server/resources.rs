//! The resources the reference conformance scenarios read.

use serde_json::json;

use chuk_mcp::server::McpServer;

use super::media;

pub fn register(server: &mut McpServer) {
    server.register_resource(
        "test://static-text",
        "static-text",
        "A static text resource",
        media::TEXT_PLAIN,
        || async { Ok("This is the content of the static text resource.".to_string()) },
    );

    server.register_binary_resource(
        "test://static-binary",
        "static-binary",
        "A static binary resource",
        media::IMAGE_PNG,
        || async { Ok(media::RED_PIXEL_PNG.to_string()) },
    );

    // The resource a subscription scenario names. It never changes; the
    // scenario is about recording the interest, not about being notified.
    server.register_resource(
        "test://watched-resource",
        "watched-resource",
        "A resource a client may subscribe to",
        media::TEXT_PLAIN,
        || async { Ok("This resource can be watched.".to_string()) },
    );

    server.register_resource_template(
        "test://template/{id}/data",
        "templated-data",
        "Data for a given id",
        media::APPLICATION_JSON,
        |bound| async move {
            let id = bound.get("id").cloned().unwrap_or_default();
            Ok(json!({
                "id": id,
                "templateTest": true,
                "data": format!("Data for ID: {id}"),
            })
            .to_string())
        },
    );
}
