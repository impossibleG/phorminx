//! Safe, synchronous access to a loopback Ollama service for transcript cleanup.
//!
//! This crate deliberately does not start Ollama or download models. It discovers
//! models already installed by the user and provides deterministic fallback
//! behavior when cleanup cannot safely be completed.

mod client;
mod model;
mod prompt;
mod validation;

pub use client::{
    CancellationToken, ClientError, ClientTimeouts, FallbackReason, FormatResult, KeepAlive,
    OllamaClient, OllamaEndpoint,
};
pub use model::{
    ModelCatalog, ModelDetails, ModelName, ModelSelectionError, OllamaModel, SelectionPolicy,
};
pub use prompt::{FormatProfile, FormatPrompt, PromptError, PromptPlan, build_prompt};
pub use validation::{
    OutputValidator, ProtectedToken, ProtectedTokens, ValidationError, ValidationPolicy,
};
