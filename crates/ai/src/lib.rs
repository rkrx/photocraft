//! Provider-independent, versioned image-generation boundary. No editor, UI, filesystem or HTTP.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use base64::Engine as _;
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const DEFAULT_MODEL: &str = "gpt-image-2.5-sunburst";
pub const MAX_IMAGE_BYTES: usize = 12 * 1024 * 1024;
pub const MAX_MASK_BYTES: usize = 4 * 1024 * 1024 - 1;
pub const MAX_EDGE: u32 = 2048;
pub const MAX_PIXELS: u64 = 4_194_304;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid image-generation request: {0}")]
    Invalid(String),
    #[error("image generation cancelled")]
    Cancelled,
    #[error("image provider: {0}")]
    Provider(String),
}
pub type Result<T> = std::result::Result<T, Error>;

/// Encoded transport data, explicitly PNG/sRGB. This is not an editor pixel API.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PngImage {
    pub width: u32,
    pub height: u32,
    pub base64: String,
}
impl PngImage {
    pub fn from_bytes(width: u32, height: u32, png: &[u8]) -> Result<Self> {
        dimensions(width, height)?;
        if png.len() > MAX_IMAGE_BYTES || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(Error::Invalid("expected a bounded PNG image".into()));
        }
        Ok(Self { width, height, base64: base64::engine::general_purpose::STANDARD.encode(png) })
    }
    pub fn bytes(&self, limit: usize) -> Result<Vec<u8>> {
        dimensions(self.width, self.height)?;
        if self.base64.len() > limit.saturating_add(2).saturating_div(3).saturating_mul(4) {
            return Err(Error::Invalid("encoded image exceeds the byte limit".into()));
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(&self.base64).map_err(|_| Error::Invalid("invalid image base64".into()))?;
        if bytes.len() > limit || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(Error::Invalid("expected a bounded PNG image".into()));
        }
        Ok(bytes)
    }
    pub fn data_url(&self) -> String {
        format!("data:image/png;base64,{}", self.base64)
    }
}
pub fn dimensions(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(Error::Invalid(format!("this milestone supports images up to {MAX_EDGE} × {MAX_EDGE} pixels")));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Fill,
}

/// No paths, credentials, engine references or editor document types cross this boundary.
/// Mask alpha is zero in the area to edit, opaque in the area to retain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub operation: Operation,
    pub prompt: String,
    pub model: String,
    pub source: PngImage,
    pub mask: PngImage,
}
impl Request {
    pub fn validate(&self) -> Result<()> {
        if self.version != VERSION {
            return Err(Error::Invalid("unsupported protocol version".into()));
        }
        if self.prompt.trim().is_empty() || self.prompt.chars().count() > 32_000 {
            return Err(Error::Invalid("prompt must contain 1–32000 characters".into()));
        }
        if self.model.is_empty() || self.model.len() > 128 || !self.model.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') {
            return Err(Error::Invalid("invalid model identifier".into()));
        }
        if (self.source.width, self.source.height) != (self.mask.width, self.mask.height) {
            return Err(Error::Invalid("image and mask dimensions must match".into()));
        }
        self.source.bytes(MAX_IMAGE_BYTES)?;
        self.mask.bytes(MAX_MASK_BYTES)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u32,
    pub image: PngImage,
}

/// Local implementations and future IPC clients implement this same interface.
/// Cancellation must prevent result application; it need not stop remote billing.
pub trait Backend: Send + Sync {
    fn id(&self) -> &'static str;
    fn generate(&self, request: &Request, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<Response>;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Request {
        let png = PngImage::from_bytes(1, 1, b"\x89PNG\r\n\x1a\n").unwrap();
        Request { version: VERSION, operation: Operation::Fill, prompt: "red flower".into(), model: DEFAULT_MODEL.into(), source: png.clone(), mask: png }
    }
    #[test]
    fn wire_round_trip_and_validation() {
        let r = request();
        r.validate().unwrap();
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&r).unwrap()).unwrap(), r);
        let mut bad = r.clone();
        bad.version += 1;
        assert!(bad.validate().is_err());
        bad = r.clone();
        bad.prompt.clear();
        assert!(bad.validate().is_err());
        bad = r.clone();
        bad.mask.width = 2;
        assert!(bad.validate().is_err());
        bad = r.clone();
        bad.source.base64 = "?".into();
        assert!(bad.validate().is_err());
        bad = r;
        bad.source.width = u32::MAX;
        assert!(bad.validate().is_err());
        assert!(dimensions(0, 1).is_err());
        assert!(PngImage::from_bytes(1, 1, b"not PNG").is_err());
    }
}
