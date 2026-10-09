//! Built-in OpenAI image backend. Editor integration depends only on `photocraft-ai`.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::OpenAi;

/// Hosts may opt into the already configured API credential without exposing it to commands.
#[cfg(not(target_arch = "wasm32"))]
pub fn from_env() -> photocraft_ai::Result<Option<std::sync::Arc<dyn photocraft_ai::Backend>>> {
    match std::env::var("OPENAI_API_KEY") {
        Ok(key) if !key.trim().is_empty() => Ok(Some(std::sync::Arc::new(OpenAi::new(&key)?))),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(photocraft_ai::Error::Invalid("OPENAI_API_KEY is not valid text".into())),
    }
}
