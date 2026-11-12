//! Cooperative cancellation and progress for offline renders.
//!
//! A render polls a [`CancelToken`] between source chunks and returns the
//! engine's cancellation message. Callers that need a typed outcome use
//! [`RenderError`], which separates a cancelled render from a failed one.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The message every cancelled render path returns.
pub const CANCELLED_MESSAGE: &str = "render cancelled";

/// A cancellation flag shared by the caller and one offline render.
///
/// Cloning shares the flag. A render polls it with a relaxed load, so the check
/// takes no lock and allocates nothing.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A fresh, uncancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Share a flag the caller already owns.
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    /// Request cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// The raw flag for renderers that only know `AtomicBool`.
    pub fn atomic(&self) -> &AtomicBool {
        &self.0
    }
}

/// Why an offline render stopped.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RenderError {
    #[error("render cancelled")]
    Cancelled,
    #[error("{0}")]
    Message(String),
}

/// Where an offline render is in its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStage {
    Rendering,
    Mixing,
}

/// One progress report: the stage and how much of it is done, `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderProgress {
    pub stage: RenderStage,
    pub fraction: f32,
}

/// Progress sink the engine calls between track renders.
pub type ProgressCallback = Box<dyn Fn(RenderProgress) + Send + Sync>;

/// True when `token` asks the render to stop.
///
/// Free function so callers can check a cloned token while a field of the cache
/// is borrowed mutably.
pub(crate) fn stopped(token: Option<&CancelToken>) -> bool {
    token.is_some_and(CancelToken::is_cancelled)
}

/// The engine's cancellation error for a `Result<_, String>` render path.
pub(crate) fn cancelled() -> String {
    CANCELLED_MESSAGE.to_string()
}
