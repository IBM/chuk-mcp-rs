//! The sample media the reference scenarios ask a server to return.
//!
//! Kept as small as the formats allow: the scenarios check that an image
//! arrives as an image, not what is in it.

/// A 1×1 red pixel, PNG.
pub const RED_PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// A silent WAV: a 44-byte header and no samples.
pub const SILENT_WAV: &str = "UklGRiQAAABXQVZFZm10IBAAAAABAAEARKwAAIhYAQACABAAZGF0YQAAAAA=";

/// Media types the fixtures declare.
pub const IMAGE_PNG: &str = "image/png";
pub const AUDIO_WAV: &str = "audio/wav";
pub const TEXT_PLAIN: &str = "text/plain";
pub const APPLICATION_JSON: &str = "application/json";
