//! MCP message method names, mirroring `chuk_mcp.protocol.messages.message_method`.

/// Available message methods in the MCP protocol.
pub struct MessageMethod;

impl MessageMethod {
    // Core protocol methods
    pub const PING: &'static str = "ping";
    pub const INITIALIZE: &'static str = "initialize";

    // Resource methods
    pub const RESOURCES_LIST: &'static str = "resources/list";
    pub const RESOURCES_READ: &'static str = "resources/read";
    pub const RESOURCES_SUBSCRIBE: &'static str = "resources/subscribe";
    pub const RESOURCES_UNSUBSCRIBE: &'static str = "resources/unsubscribe";
    pub const RESOURCES_TEMPLATES_LIST: &'static str = "resources/templates/list";

    // Tool methods
    pub const TOOLS_LIST: &'static str = "tools/list";
    pub const TOOLS_CALL: &'static str = "tools/call";

    // Prompt methods
    pub const PROMPTS_LIST: &'static str = "prompts/list";
    pub const PROMPTS_GET: &'static str = "prompts/get";

    // Logging methods
    pub const LOGGING_SET_LEVEL: &'static str = "logging/setLevel";

    // Completion methods
    pub const COMPLETION_COMPLETE: &'static str = "completion/complete";

    // Sampling methods (client features)
    pub const SAMPLING_CREATE_MESSAGE: &'static str = "sampling/createMessage";

    // Roots methods (client features)
    pub const ROOTS_LIST: &'static str = "roots/list";

    // Elicitation methods (client features)
    pub const ELICITATION_CREATE: &'static str = "elicitation/create";

    // Notification methods
    pub const NOTIFICATION_INITIALIZED: &'static str = "notifications/initialized";
    pub const NOTIFICATION_CANCELLED: &'static str = "notifications/cancelled";
    pub const NOTIFICATION_PROGRESS: &'static str = "notifications/progress";
    pub const NOTIFICATION_MESSAGE: &'static str = "notifications/message";

    // Resource notifications
    pub const NOTIFICATION_RESOURCES_LIST_CHANGED: &'static str =
        "notifications/resources/list_changed";
    pub const NOTIFICATION_RESOURCES_UPDATED: &'static str = "notifications/resources/updated";

    // Prompt notifications
    pub const NOTIFICATION_PROMPTS_LIST_CHANGED: &'static str =
        "notifications/prompts/list_changed";

    // Tool notifications
    pub const NOTIFICATION_TOOLS_LIST_CHANGED: &'static str = "notifications/tools/list_changed";

    // Roots notifications
    pub const NOTIFICATION_ROOTS_LIST_CHANGED: &'static str = "notifications/roots/list_changed";
}
