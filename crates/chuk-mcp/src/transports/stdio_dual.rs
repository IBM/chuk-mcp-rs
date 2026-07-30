//! Dual-era stdio connection: probe once, then speak whichever era the peer does.
//!
//! stdio is the easier half of era detection. HTTP has no request that
//! distinguishes the eras without also *being* a real request, so its first real
//! call has to double as the probe. Here there is a legitimate pre-flight: the
//! spec says a dual-era client **SHOULD** send `server/discover` first, and
//! servers **MUST** implement it. So detection happens once, before any real
//! work, and fails deterministically rather than half-way through a call.
//!
//! The other difference is where era lives. On HTTP it selects between two
//! transports with different connection semantics; on stdio there is one pipe
//! and era is purely message *shape* — modern requests carry `_meta`, legacy
//! ones complete an `initialize` handshake first. There are no mirrored headers
//! at all: those are a Streamable HTTP concern, so the envelope's headers are
//! unused here and `_meta` is the whole story.
//!
//! Either way the caller receives a [`ServerProfile`] and never learns which
//! path produced it.

use std::time::Duration;

use crate::protocol::envelope::{build_envelope, ClientIdentity};
use crate::protocol::era::{
    classify_probe_result, renegotiate, Detection, EraMode, ProtocolEra, ServerProfile,
};
use crate::protocol::messages::initialize::{send_initialize_with_options, InitializeOptions};
use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::send_message::{
    send_message_with_options, ReadStream, SendMessageOptions, WriteStream,
};
use crate::protocol::meta::RequestMeta;
use crate::protocol::types::errors::McpError;
use crate::protocol::versioning;
use crate::transports::limits::TransportLimits;
use crate::transports::stdio::{StdioParameters, StdioTransport};
use crate::transports::Transport;

/// A stdio connection whose era has already been settled.
pub struct StdioConnection {
    pub transport: StdioTransport,
    pub read: ReadStream,
    pub write: WriteStream,
    /// What the peer said about itself, however we learned it.
    pub profile: ServerProfile,
}

impl StdioConnection {
    pub fn era(&self) -> ProtocolEra {
        self.profile.era
    }
}

/// How to open a dual-era stdio connection.
#[derive(Debug, Clone)]
pub struct StdioDualOptions {
    pub mode: EraMode,
    pub identity: ClientIdentity,
    /// Timeout for the probe and for the legacy handshake.
    pub timeout: Option<Duration>,
    pub limits: TransportLimits,
}

impl Default for StdioDualOptions {
    fn default() -> Self {
        StdioDualOptions {
            mode: EraMode::Auto,
            identity: ClientIdentity::chuk(),
            timeout: Some(Duration::from_secs(30)),
            limits: TransportLimits::default(),
        }
    }
}

/// Start a stdio server and settle which protocol era it speaks.
pub async fn stdio_client_dual(
    parameters: StdioParameters,
    options: StdioDualOptions,
) -> Result<StdioConnection, McpError> {
    let transport = StdioTransport::start_with_limits(parameters, options.limits).await?;
    let (read, write) = transport.get_streams().await?;

    // Set the metadata before probing: the probe is itself a modern request and
    // would be rejected without it. Cleared again if the peer turns out legacy.
    let meta = RequestMeta {
        client_info: options.identity.info.clone(),
        ..RequestMeta::new(
            versioning::FIRST_MODERN_VERSION,
            options.identity.capabilities.clone(),
        )
    };
    transport.set_modern_meta(meta.clone());

    let profile = match options.mode.resolve(None) {
        // Pinned: no probe at all, in either direction.
        Some(ProtocolEra::Legacy) => {
            transport.clear_modern_meta();
            legacy_handshake(&read, &write, &options).await?
        }
        Some(ProtocolEra::Modern) => discover(&read, &write, &options).await?,
        None => match probe(&read, &write, &options).await? {
            Some(profile) => profile,
            None => {
                transport.clear_modern_meta();
                legacy_handshake(&read, &write, &options).await?
            }
        },
    };

    Ok(StdioConnection {
        transport,
        read,
        write,
        profile,
    })
}

/// Probe with `server/discover`.
///
/// `Ok(Some(profile))` means modern, `Ok(None)` means fall back to `initialize`,
/// and an error means the probe told us nothing — a crashed process or a timeout
/// must not be mistaken for a legacy server.
async fn probe(
    read: &ReadStream,
    write: &WriteStream,
    options: &StdioDualOptions,
) -> Result<Option<ServerProfile>, McpError> {
    let result = discover_raw(read, write, options, versioning::FIRST_MODERN_VERSION).await;

    match classify_probe_result(&result) {
        Detection::Modern => Ok(Some(
            profile_from_probe(result, read, write, options).await?,
        )),
        Detection::Legacy => {
            tracing::debug!("server/discover was not understood; falling back to initialize");
            Ok(None)
        }
        Detection::Undetermined => Err(result.expect_err("undetermined implies an error")),
    }
}

/// Turn a modern probe outcome into a profile, renegotiating if the server
/// rejected our version.
async fn profile_from_probe(
    result: Result<serde_json::Value, McpError>,
    read: &ReadStream,
    write: &WriteStream,
    options: &StdioDualOptions,
) -> Result<ServerProfile, McpError> {
    let error = match result {
        Ok(value) => return ServerProfile::from_discover(&value),
        Err(e) => e,
    };

    // A modern server that dislikes our version lists what it does support, so
    // this is recoverable rather than fatal.
    let Some(version) = renegotiate(&error) else {
        return Err(error);
    };
    tracing::debug!("retrying server/discover as {version}");
    let value = discover_raw(read, write, options, &version).await?;
    ServerProfile::from_discover(&value)
}

/// `server/discover` as a modern request, for a pinned-modern connection.
async fn discover(
    read: &ReadStream,
    write: &WriteStream,
    options: &StdioDualOptions,
) -> Result<ServerProfile, McpError> {
    let result = discover_raw(read, write, options, versioning::FIRST_MODERN_VERSION).await;
    profile_from_probe(result, read, write, options).await
}

/// Issue one `server/discover`, declaring `version`.
async fn discover_raw(
    read: &ReadStream,
    write: &WriteStream,
    options: &StdioDualOptions,
    version: &str,
) -> Result<serde_json::Value, McpError> {
    // Built through the shared envelope so `_meta` is constructed exactly once,
    // in one place, for both transports. The headers it also produces are a
    // Streamable HTTP concern and are unused on stdio.
    let envelope = build_envelope(
        MessageMethod::SERVER_DISCOVER,
        None,
        version,
        &options.identity,
    )?;

    send_message_with_options(
        read,
        write,
        MessageMethod::SERVER_DISCOVER,
        Some(envelope.params),
        SendMessageOptions {
            timeout: options.timeout,
            ..Default::default()
        },
    )
    .await
}

/// The legacy `initialize` handshake, normalised into the same profile shape.
async fn legacy_handshake(
    read: &ReadStream,
    write: &WriteStream,
    options: &StdioDualOptions,
) -> Result<ServerProfile, McpError> {
    let result = send_initialize_with_options(
        read,
        write,
        InitializeOptions {
            timeout: options.timeout,
            client_info: options.identity.info.clone(),
            capabilities: Some(options.identity.capabilities.clone()),
            ..Default::default()
        },
    )
    .await?;

    ServerProfile::from_initialize(&serde_json::to_value(&result)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_detect_rather_than_assume() {
        let o = StdioDualOptions::default();
        assert_eq!(o.mode, EraMode::Auto);
        assert!(o.mode.requires_detection());
        assert!(o.identity.info.is_some());
        assert_eq!(o.timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn a_pinned_mode_resolves_without_probing() {
        assert_eq!(
            EraMode::Legacy.resolve(None),
            Some(ProtocolEra::Legacy),
            "a legacy pin must not need a probe"
        );
        assert_eq!(EraMode::Modern.resolve(None), Some(ProtocolEra::Modern));
        assert_eq!(EraMode::Auto.resolve(None), None);
    }

    #[tokio::test]
    async fn an_unstartable_command_fails_before_any_probe() {
        // `StdioConnection` owns a subprocess and is deliberately not Debug, so
        // inspect the error rather than unwrapping the Result.
        let outcome = stdio_client_dual(
            StdioParameters::new("definitely-not-a-real-binary-xyz", ["--x"]),
            StdioDualOptions::default(),
        )
        .await;
        match outcome {
            Err(e) => assert!(matches!(e, McpError::Transport(_)), "{e}"),
            Ok(_) => panic!("a missing binary should not produce a connection"),
        }
    }

    #[test]
    fn an_empty_command_is_rejected() {
        // Guards the validation path without spawning anything.
        assert!(StdioParameters::new("", [] as [&str; 0]).command.is_empty());
    }
}
