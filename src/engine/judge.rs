//! The seam between *preparing* a review and *judging* it (#649).
//!
//! A review is three steps:
//!
//! 1. [`review::prepare`](crate::engine::review::prepare) turns a diff into a
//!    [`Packet`]: everything a judge needs (the text it should see, the
//!    assembled context, the system prompt override) plus everything the
//!    finishing step needs (`valid_files`, the diff chunks, the deterministic
//!    findings).
//! 2. A [`Judge`] reads the packet and returns findings. This is the seam.
//! 3. [`review::review_with`](crate::engine::review::review_with) merges the
//!    deterministic findings, guards the judge's file paths, post-processes,
//!    and decides `should_block`.
//!
//! Adapters at the seam: [`HttpJudge`] (an OpenAI-compatible LLM, today's
//! behavior), [`NoJudge`] (deterministic checks only, no LLM), and, later, an
//! agent that judges for itself. A fake `Judge` makes the whole pipeline
//! testable without a network.

use crate::config::schema::Config;
use crate::engine::deterministic::DeterministicReport;
use crate::engine::diff_parser::FileChunk;
use crate::engine::llm;
use crate::engine::types::{LLMConfig, ReviewResponse};
use crate::error::CoraError;

/// Everything about one review that does not depend on who judges it.
#[derive(Debug)]
pub struct Packet {
    /// The diff text a judge sees (comment-sanitized when configured).
    pub diff_text: String,
    /// Assembled prompt context (static analysis, deterministic findings,
    /// cross-file, language, profile, memory, brain), if any.
    pub context: Option<String>,
    /// Resolved system prompt override, if any.
    pub system_prompt: Option<String>,
    /// File paths present in the diff; a judge may only report on these.
    pub valid_files: Vec<String>,
    /// Parsed diff of the ORIGINAL (unsanitized) text.
    pub chunks: Vec<FileChunk>,
    /// Findings of every deterministic scanner family.
    pub deterministic: DeterministicReport,
}

/// Something that turns a [`Packet`] into findings.
pub trait Judge {
    async fn judge(&self, config: &Config, packet: &Packet) -> Result<ReviewResponse, CoraError>;
}

/// Judge backed by an OpenAI-compatible LLM.
pub struct HttpJudge<'a> {
    pub llm_config: &'a LLMConfig,
    /// Stream tokens to stdout as they arrive.
    pub stream: bool,
    /// No spinner (ignored when streaming).
    pub quiet: bool,
}

impl Judge for HttpJudge<'_> {
    async fn judge(&self, config: &Config, packet: &Packet) -> Result<ReviewResponse, CoraError> {
        if self.stream {
            return llm::review_diff_stream(
                self.llm_config,
                &packet.diff_text,
                &config.focus,
                &config.rules,
                &config.response_format,
                packet.system_prompt.as_deref(),
                packet.context.as_deref(),
                &crate::progress::StdoutStream,
            )
            .await
            .inspect(|_| println!()); // trailing newline after streamed output
        }
        let spinner = crate::progress::Spinner::new("Reviewing diff…", self.quiet);
        let result = llm::review_diff(
            self.llm_config,
            &packet.diff_text,
            &config.focus,
            &config.rules,
            &config.response_format,
            packet.system_prompt.as_deref(),
            packet.context.as_deref(),
            &spinner,
        )
        .await;
        spinner.finish();
        result
    }
}

/// Judge that finds nothing: the review is the deterministic findings only.
// Selected by `cora review --no-llm` / a missing API key (#646).
#[allow(dead_code)]
pub struct NoJudge;

impl Judge for NoJudge {
    async fn judge(&self, _config: &Config, _packet: &Packet) -> Result<ReviewResponse, CoraError> {
        Ok(ReviewResponse {
            issues: Vec::new(),
            summary: "Deterministic checks only (no LLM judge).".to_string(),
            tokens_used: None,
            should_block: false,
            dropped_findings: 0,
        })
    }
}
