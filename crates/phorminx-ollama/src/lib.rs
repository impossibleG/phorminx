//! Safe, synchronous access to a loopback Ollama service for transcript cleanup.
//!
//! This crate deliberately does not start Ollama or download models. It discovers
//! models already installed by the user and provides deterministic fallback
//! behavior when cleanup cannot safely be completed.

mod client;
mod document;
mod model;
mod onboarding;
mod prompt;
mod validation;

pub use client::{
    CancellationToken, ClientError, ClientTimeouts, FallbackReason, FormatResult, KeepAlive,
    OllamaClient, OllamaEndpoint,
};
pub use document::{
    DocumentChunk, DocumentChunkError, DocumentChunkOutcome, DocumentChunkPolicy,
    DocumentChunkReport, DocumentFormatDisposition, DocumentFormatError, DocumentFormatResult,
    DocumentOutcomeCounts, MAXIMUM_DOCUMENT_OUTPUT_BYTES, chunk_document, reconstruct_document,
};
pub use model::{
    ModelCatalog, ModelDetails, ModelName, ModelSelectionError, OllamaModel, SelectionPolicy,
};
pub use onboarding::{
    AuthorizedModelPull, CuratedLanguage, CuratedModel, CuratedModelCatalog, CuratedModelId,
    CuratedModelState, DaemonState, InstalledModelIdentity, ModelPullOutcome, ModelPullProgress,
    ModelPullReview, OllamaOnboarding, OllamaOnboardingTransport, OllamaVersion, PullFailure,
    PullResidue, TransportError, UreqOnboardingTransport,
};
pub use prompt::{FormatProfile, FormatPrompt, PromptError, PromptPlan, build_prompt};
pub use validation::{
    OutputValidator, ProtectedToken, ProtectedTokens, ValidationError, ValidationPolicy,
};
