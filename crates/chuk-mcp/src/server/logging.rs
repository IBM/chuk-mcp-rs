//! How much the client has asked to hear.
//!
//! `logging/setLevel` sets a floor; the server sends nothing below it. The
//! levels are RFC 5424's, which is where the specification takes them from.

use std::sync::Mutex;

/// A severity, least severe first. The order is the whole point of the type:
/// "at least this severe" is a comparison, not a lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug,
    Info,
    Notice,
    Warning,
    Error,
    Critical,
    Alert,
    Emergency,
}

/// What the client is assumed to want before it says otherwise.
pub const DEFAULT_LEVEL: LogLevel = LogLevel::Info;

impl LogLevel {
    /// Every level, least severe first.
    pub const ALL: [LogLevel; 8] = [
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Notice,
        LogLevel::Warning,
        LogLevel::Error,
        LogLevel::Critical,
        LogLevel::Alert,
        LogLevel::Emergency,
    ];

    /// The name this level goes by on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Notice => "notice",
            LogLevel::Warning => "warning",
            LogLevel::Error => "error",
            LogLevel::Critical => "critical",
            LogLevel::Alert => "alert",
            LogLevel::Emergency => "emergency",
        }
    }

    /// Read a level by name, or `None` if it names no level.
    ///
    /// An unrecognised name is refused rather than defaulted: silently storing
    /// one would leave the client believing a filter is in place that is not.
    pub fn parse(name: &str) -> Option<Self> {
        LogLevel::ALL
            .into_iter()
            .find(|level| level.as_str() == name)
    }
}

/// The level currently in force, which any connection may change.
#[derive(Debug)]
pub struct LogLevelSetting {
    level: Mutex<LogLevel>,
}

impl Default for LogLevelSetting {
    fn default() -> Self {
        LogLevelSetting {
            level: Mutex::new(DEFAULT_LEVEL),
        }
    }
}

impl LogLevelSetting {
    pub fn new() -> Self {
        Self::default()
    }

    /// The level in force.
    pub fn get(&self) -> LogLevel {
        *self
            .level
            .lock()
            .expect("the log level lock is never held across a panic")
    }

    /// Set the level in force.
    pub fn set(&self, level: LogLevel) {
        *self
            .level
            .lock()
            .expect("the log level lock is never held across a panic") = level;
    }

    /// Whether a message at this level would be sent.
    pub fn permits(&self, level: LogLevel) -> bool {
        level >= self.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_level_round_trips_through_its_name() {
        for level in LogLevel::ALL {
            assert_eq!(LogLevel::parse(level.as_str()), Some(level));
        }
    }

    #[test]
    fn a_name_that_is_not_a_level_is_refused() {
        assert_eq!(LogLevel::parse("chatty"), None);
        assert_eq!(LogLevel::parse(""), None);
        // Names are exact; the wire format has no case folding.
        assert_eq!(LogLevel::parse("Debug"), None);
    }

    #[test]
    fn severity_orders_least_to_most() {
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warning);
        assert!(LogLevel::Warning < LogLevel::Error);
        assert!(LogLevel::Error < LogLevel::Emergency);
    }

    #[test]
    fn the_setting_starts_at_the_default_and_can_be_changed() {
        let setting = LogLevelSetting::new();
        assert_eq!(setting.get(), DEFAULT_LEVEL);

        setting.set(LogLevel::Error);
        assert_eq!(setting.get(), LogLevel::Error);
    }

    #[test]
    fn a_level_below_the_floor_is_not_sent() {
        let setting = LogLevelSetting::new();
        setting.set(LogLevel::Warning);

        assert!(!setting.permits(LogLevel::Debug));
        assert!(!setting.permits(LogLevel::Info));
        // The floor itself is included: "at least this severe".
        assert!(setting.permits(LogLevel::Warning));
        assert!(setting.permits(LogLevel::Error));
    }
}
