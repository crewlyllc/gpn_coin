//! LLM bridge — connects the GPN compute marketplace to a local inference
//! runtime (ollama / llama.cpp, OpenAI-compatible).
//!
//! This is the "bridge" between the chain (coordination + escrow, which only
//! carries job state and result *hashes*) and the actual model inference (which
//! runs off-chain against a local ollama server). It provides:
//!
//! - [`store`] — an off-chain store for LLM job payloads (prompt, model, params,
//!   response), keyed by on-chain job id. The chain never sees the prompt; the
//!   requestor writes it here and the provider reads it here. (On a single
//!   machine this is a local JSON file; in a distributed deployment this would
//!   be a shared object store or a direct requestor<->provider channel.)
//! - [`infer`] — a thin async client for the ollama OpenAI-compatible
//!   `/v1/chat/completions` endpoint, returning the completion text and a
//!   sha256 result hash (which is what gets submitted on-chain).

pub mod infer;
pub mod store;

pub use infer::{normalize_ml_task, run_inference, run_ml_inference};
pub use store::{get_job, put_job, set_response, LlmJob, LlmStore};
