//! Optional embedding layer for the Project Index.
//!
//! When the user sets an embedding model in the Prompt Enhancer settings,
//! `build_local` / `build_remote` will call `embed_batch` to vectorize
//! each file's summary and persist the vectors alongside the index. At
//! retrieval time, the prompt is embedded once and cosine-similarity is
//! folded into the file/chunk scores.
//!
//! Per CLAUDE.md `--ignore-scripts` / no-new-deps stance, this module
//! reuses the existing pinned `reqwest` dependency. No new crates added.

use crate::errors::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// OpenAI-compatible /embeddings limit shared by most providers. Stay
/// conservative so slow providers don't timeout us mid-index.
pub const MAX_BATCH: usize = 32;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Provider-agnostic embedding configuration. Provider field mirrors the
/// `PromptEnhancerProvider` enum so callers can pass the same shape they
/// already have for chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub provider: String,
    pub base_url: Option<String>,
    pub api_key: String,
    pub embedding_model: String,
}

impl EmbeddingConfig {
    pub fn validate(&self) -> AppResult<()> {
        if self.api_key.trim().is_empty() {
            return Err(AppError::Invalid("embedding api_key required".into()));
        }
        if self.embedding_model.trim().is_empty() {
            return Err(AppError::Invalid("embedding_model required".into()));
        }
        if self.provider == "deepseek" {
            return Err(AppError::Invalid(
                "DeepSeek direct has no embedding API. Use OpenRouter or Custom.".into(),
            ));
        }
        let _ = self.resolved_base_url()?;
        Ok(())
    }

    pub fn resolved_base_url(&self) -> AppResult<String> {
        let raw = match self.provider.as_str() {
            "openrouter" => self
                .base_url
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("https://openrouter.ai/api/v1")
                .to_string(),
            "mimo" | "custom" => self
                .base_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    AppError::Invalid("base_url required for this embedding provider".into())
                })?
                .to_string(),
            "deepseek" => {
                return Err(AppError::Invalid("DeepSeek has no embeddings".into()));
            }
            other => {
                return Err(AppError::Invalid(format!(
                    "unknown embedding provider: {other}"
                )));
            }
        };
        let trimmed = raw.trim().trim_end_matches('/').to_string();
        if !(trimmed.starts_with("https://")
            || trimmed.starts_with("http://localhost")
            || trimmed.starts_with("http://127.0.0.1"))
        {
            return Err(AppError::Invalid(
                "embedding base URL must be https or localhost".into(),
            ));
        }
        Ok(trimmed)
    }
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedDatum>,
}

#[derive(Deserialize)]
struct EmbedDatum {
    embedding: Vec<f32>,
}

/// Embed up to MAX_BATCH inputs in one HTTP round-trip. Returns vectors
/// in the same order as `inputs`. Errors propagate.
pub async fn embed_batch(ai: &EmbeddingConfig, inputs: &[String]) -> AppResult<Vec<Vec<f32>>> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    if inputs.len() > MAX_BATCH {
        return Err(AppError::Invalid(format!(
            "embed batch over limit ({} > {MAX_BATCH})",
            inputs.len()
        )));
    }
    ai.validate()?;
    let base = ai.resolved_base_url()?;
    let endpoint = format!("{}/embeddings", base);
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| AppError::Internal(format!("embed client: {e}")))?;
    let body = EmbedRequest {
        model: ai.embedding_model.trim(),
        input: inputs,
    };
    let mut builder = client
        .post(&endpoint)
        .bearer_auth(ai.api_key.trim())
        .json(&body);
    if ai.provider == "openrouter" {
        builder = builder
            .header("HTTP-Referer", "https://tersh.app")
            .header("X-Title", "Tersh");
    }
    let resp = builder
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("embed request: {e}")))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::Internal(format!("embed read: {e}")))?;
    if !status.is_success() {
        let short = text.chars().take(400).collect::<String>();
        return Err(AppError::Invalid(format!(
            "embedding provider returned {status}: {short}"
        )));
    }
    let parsed: EmbedResponse =
        serde_json::from_str(&text).map_err(|e| AppError::Internal(format!("embed parse: {e}")))?;
    if parsed.data.len() != inputs.len() {
        return Err(AppError::Internal(format!(
            "embedding provider returned {} vectors for {} inputs",
            parsed.data.len(),
            inputs.len()
        )));
    }
    Ok(parsed.data.into_iter().map(|d| d.embedding).collect())
}

/// Embed all inputs respecting MAX_BATCH chunking. Failures abort the
/// whole batch and propagate (so a partial embedding never silently
/// produces a half-vectorized index).
pub async fn embed_all(ai: &EmbeddingConfig, inputs: Vec<String>) -> AppResult<Vec<Vec<f32>>> {
    let mut out: Vec<Vec<f32>> = Vec::with_capacity(inputs.len());
    for batch in inputs.chunks(MAX_BATCH) {
        let vectors = embed_batch(ai, batch).await?;
        out.extend(vectors);
    }
    Ok(out)
}

/// Cosine similarity between two vectors. Returns 0.0 for shape
/// mismatch or zero magnitude.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot: f32 = 0.0;
    let mut na: f32 = 0.0;
    let mut nb: f32 = 0.0;
    for i in 0..a.len() {
        let x = a[i];
        let y = b[i];
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= f32::EPSILON || nb <= f32::EPSILON {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_vectors_is_one() {
        let v = vec![0.1, 0.2, 0.3];
        let score = cosine_similarity(&v, &v);
        assert!((score - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-5);
    }

    #[test]
    fn cosine_mismatched_lengths_returns_zero() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn config_rejects_missing_fields() {
        let bad = EmbeddingConfig {
            provider: "openrouter".into(),
            base_url: None,
            api_key: "".into(),
            embedding_model: "openai/text-embedding-3-small".into(),
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn config_rejects_deepseek() {
        let bad = EmbeddingConfig {
            provider: "deepseek".into(),
            base_url: None,
            api_key: "sk-x".into(),
            embedding_model: "deepseek-v4-flash".into(),
        };
        assert!(bad.validate().is_err());
    }
}
