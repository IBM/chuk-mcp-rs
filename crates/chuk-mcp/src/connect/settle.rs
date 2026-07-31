//! Settling which era a peer speaks, over an already-started transport.
//!
//! Stdio has its own settling routine ([`crate::transports::stdio_dual`]),
//! because it can probe before anyone is watching. HTTP cannot: the first real
//! request is the probe. This module is that first request, issued deliberately
//! so the high-level client knows what it is talking to before the caller's
//! traffic starts.

use std::time::Duration;

use crate::protocol::envelope::{build_envelope, ClientIdentity};
use crate::protocol::era::{classify_probe_error, Detection, EraMode, ProtocolEra, ServerProfile};
use crate::protocol::messages::initialize::{send_initialize_with_options, InitializeOptions};
use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::send_message::{
    send_message_with_options, ReadStream, SendMessageOptions, WriteStream,
};
use crate::protocol::types::errors::McpError;
use crate::protocol::versioning;

/// Settle the era over a stream pair and return the peer's profile.
///
/// Under [`EraMode::Auto`] this issues `server/discover` and falls back to
/// `initialize` only when the failure *proves* the peer is not modern. A
/// timeout or a transport failure proves nothing, and is reported as the error
/// it is rather than guessed into a legacy handshake.
pub async fn settle(
    read: &ReadStream,
    write: &WriteStream,
    mode: EraMode,
    identity: &ClientIdentity,
    timeout: Option<Duration>,
) -> Result<ServerProfile, McpError> {
    match mode.resolve(None) {
        Some(ProtocolEra::Legacy) => legacy_handshake(read, write, identity, timeout).await,
        Some(ProtocolEra::Modern) => discover(read, write, identity, timeout).await,
        None => match discover(read, write, identity, timeout).await {
            Ok(profile) => Ok(profile),
            Err(error) => match classify_probe_error(&error) {
                Detection::Legacy => legacy_handshake(read, write, identity, timeout).await,
                // Modern-but-failed, or nothing learned: both mean the caller
                // should see the error, not a second guess at the lifecycle.
                Detection::Modern | Detection::Undetermined => Err(error),
            },
        },
    }
}

/// One `server/discover`, declared as a modern request.
async fn discover(
    read: &ReadStream,
    write: &WriteStream,
    identity: &ClientIdentity,
    timeout: Option<Duration>,
) -> Result<ServerProfile, McpError> {
    // Built through the shared envelope so `_meta` is constructed in exactly
    // one place for every transport.
    let envelope = build_envelope(
        MessageMethod::SERVER_DISCOVER,
        None,
        versioning::FIRST_MODERN_VERSION,
        identity,
    )?;

    let result = send_message_with_options(
        read,
        write,
        MessageMethod::SERVER_DISCOVER,
        Some(envelope.params),
        SendMessageOptions {
            timeout,
            ..Default::default()
        },
    )
    .await?;

    ServerProfile::from_discover(&result)
}

/// The legacy handshake, normalised into the same profile shape.
async fn legacy_handshake(
    read: &ReadStream,
    write: &WriteStream,
    identity: &ClientIdentity,
    timeout: Option<Duration>,
) -> Result<ServerProfile, McpError> {
    let result = send_initialize_with_options(
        read,
        write,
        InitializeOptions {
            timeout,
            client_info: identity.info.clone(),
            capabilities: Some(identity.capabilities.clone()),
            ..Default::default()
        },
    )
    .await?;

    ServerProfile::from_initialize(&serde_json::to_value(&result)?)
}
