//! The prompts the reference conformance scenarios render.

use serde_json::Value;

use chuk_mcp::server::prompts::{image_message, prompt_argument, resource_message, text_message};
use chuk_mcp::server::McpServer;

use super::media;

/// The role every fixture prompt speaks as.
const USER: &str = "user";

/// The text an argument map holds under `name`.
fn argument(arguments: &serde_json::Map<String, Value>, name: &str) -> String {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn register(server: &mut McpServer) {
    server.register_prompt(
        "test_simple_prompt",
        "A simple prompt with no arguments",
        vec![],
        |_| async {
            Ok(vec![text_message(
                USER,
                "This is a simple prompt for testing.",
            )])
        },
    );

    server.register_prompt(
        "test_prompt_with_arguments",
        "A prompt taking two arguments",
        vec![
            prompt_argument("arg1", "First test argument", true),
            prompt_argument("arg2", "Second test argument", true),
        ],
        |arguments| async move {
            Ok(vec![text_message(
                USER,
                format!(
                    "Prompt with arguments: arg1='{}', arg2='{}'",
                    argument(&arguments, "arg1"),
                    argument(&arguments, "arg2"),
                ),
            )])
        },
    );

    server.register_prompt(
        "test_prompt_with_embedded_resource",
        "A prompt embedding a resource",
        vec![prompt_argument(
            "resourceUri",
            "URI of the resource to embed",
            true,
        )],
        |arguments| async move {
            Ok(vec![
                resource_message(
                    USER,
                    argument(&arguments, "resourceUri"),
                    media::TEXT_PLAIN,
                    "Embedded resource content for testing.",
                ),
                text_message(USER, "Please process the embedded resource above."),
            ])
        },
    );

    server.register_prompt(
        "test_prompt_with_image",
        "A prompt carrying an image",
        vec![],
        |_| async {
            Ok(vec![
                image_message(USER, media::RED_PIXEL_PNG, media::IMAGE_PNG),
                text_message(USER, "Please describe the image above."),
            ])
        },
    );
}
