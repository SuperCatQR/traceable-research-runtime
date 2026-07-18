//! Stable errors for the runtime's public command Interface.

use rusqlite::Error as SqliteError;
use serde::{Deserialize, Serialize};
use std::fmt::Display;

/// A coarse stage at which a command failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStage {
    /// Input and configuration validation.
    Setup,
    /// Domain normalization and lifecycle state transitions.
    Lifecycle,
    /// Markdown corpus publication or reading.
    Corpus,
    /// Model task dispatch and response validation.
    Model,
    /// Fixed research execution orchestration.
    Execution,
    /// Event append and replay.
    Trace,
    /// Public or audit projection.
    Projection,
    /// Persistent storage.
    Storage,
}

/// Stable machine-readable error categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeErrorCode {
    /// The object is either absent or inaccessible to the principal.
    ObjectNotAvailable,
    /// A request field or model response violates a domain contract.
    ValidationFailed,
    /// A command conflicts with an already committed command or state.
    Conflict,
    /// The current lifecycle state does not allow the requested command.
    InvalidState,
    /// A persisted event stream or snapshot failed integrity checks.
    CorruptState,
    /// The configured resource budget cannot accommodate the operation.
    LimitExceeded,
    /// A model transport or endpoint failure occurred.
    ModelTransport,
    /// A model response was not valid for its closed task schema.
    ModelResponse,
    /// The caller requested cancellation or a persisted cancellation won.
    Cancelled,
    /// The local persistence layer failed.
    Storage,
    /// An unexpected internal invariant failed.
    Internal,
}

/// Typed runtime failure carrying a stable code, stage and retry hint.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// An object was absent or owned by another principal. The message is
    /// deliberately generic so callers cannot use error differences to probe
    /// object existence.
    #[error("object is not available")]
    ObjectNotAvailable {
        /// The stage at which the lookup was attempted.
        stage: RuntimeStage,
    },
    /// A caller or model supplied invalid data.
    #[error("validation failed at {stage:?}: {message}")]
    Validation {
        /// The failing stage.
        stage: RuntimeStage,
        /// A safe, field-level explanation.
        message: String,
    },
    /// A command cannot be applied without changing its identity or payload.
    #[error("command conflict at {stage:?}: {message}")]
    Conflict {
        /// The failing stage.
        stage: RuntimeStage,
        /// A safe conflict explanation.
        message: String,
    },
    /// A command was valid but arrived in an incompatible lifecycle state.
    #[error("invalid state at {stage:?}: {message}")]
    InvalidState {
        /// The failing stage.
        stage: RuntimeStage,
        /// The expected/actual state explanation.
        message: String,
    },
    /// A persisted event or snapshot failed deterministic replay validation.
    #[error("corrupt state at {stage:?}: {message}")]
    CorruptState {
        /// The failing stage.
        stage: RuntimeStage,
        /// A safe integrity explanation.
        message: String,
    },
    /// A frozen execution budget was exhausted.
    #[error("execution limit exceeded: {message}")]
    LimitExceeded {
        /// The named limit and current value.
        message: String,
    },
    /// A remote model call failed before a valid response was received.
    #[error("model transport failed: {message}")]
    ModelTransport {
        /// A redacted transport explanation.
        message: String,
        /// Whether retrying the same logical task may succeed.
        retryable: bool,
    },
    /// A model response failed its closed schema or ownership validation.
    #[error("model response rejected: {message}")]
    ModelResponse {
        /// A safe validation explanation.
        message: String,
    },
    /// A research request was cancelled.
    #[error("research request cancelled")]
    Cancelled,
    /// SQLite or a local blocking storage operation failed.
    #[error("storage failed: {message}")]
    Storage {
        /// A safe storage explanation.
        message: String,
    },
    /// An internal invariant failed; the message must not contain secrets.
    #[error("internal runtime failure: {message}")]
    Internal {
        /// A diagnostic safe for local logs.
        message: String,
    },
}

impl RuntimeError {
    /// Returns the stable machine-readable category.
    #[must_use]
    pub const fn code(&self) -> RuntimeErrorCode {
        match self {
            Self::ObjectNotAvailable { .. } => RuntimeErrorCode::ObjectNotAvailable,
            Self::Validation { .. } => RuntimeErrorCode::ValidationFailed,
            Self::Conflict { .. } => RuntimeErrorCode::Conflict,
            Self::InvalidState { .. } => RuntimeErrorCode::InvalidState,
            Self::CorruptState { .. } => RuntimeErrorCode::CorruptState,
            Self::LimitExceeded { .. } => RuntimeErrorCode::LimitExceeded,
            Self::ModelTransport { .. } => RuntimeErrorCode::ModelTransport,
            Self::ModelResponse { .. } => RuntimeErrorCode::ModelResponse,
            Self::Cancelled => RuntimeErrorCode::Cancelled,
            Self::Storage { .. } => RuntimeErrorCode::Storage,
            Self::Internal { .. } => RuntimeErrorCode::Internal,
        }
    }

    /// Returns the lifecycle stage associated with the failure.
    #[must_use]
    pub const fn stage(&self) -> RuntimeStage {
        match self {
            Self::ObjectNotAvailable { stage }
            | Self::Validation { stage, .. }
            | Self::Conflict { stage, .. }
            | Self::InvalidState { stage, .. }
            | Self::CorruptState { stage, .. } => *stage,
            Self::LimitExceeded { .. } | Self::Cancelled => RuntimeStage::Execution,
            Self::ModelTransport { .. } | Self::ModelResponse { .. } => RuntimeStage::Model,
            Self::Storage { .. } => RuntimeStage::Storage,
            Self::Internal { .. } => RuntimeStage::Setup,
        }
    }

    /// Whether retrying the same logical command may be useful.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(self, Self::ModelTransport { retryable: true, .. } | Self::Storage { .. })
    }

    /// Builds a validation error without exposing internal object data.
    #[must_use]
    pub fn validation(stage: RuntimeStage, message: impl Into<String>) -> Self {
        Self::Validation { stage, message: message.into() }
    }
}

impl From<SqliteError> for RuntimeError {
    fn from(error: SqliteError) -> Self {
        Self::Storage { message: sanitize_storage_message(error.to_string()) }
    }
}

impl From<serde_json::Error> for RuntimeError {
    fn from(error: serde_json::Error) -> Self {
        Self::CorruptState {
            stage: RuntimeStage::Trace,
            message: format!("invalid JSON payload: {error}"),
        }
    }
}

impl From<std::io::Error> for RuntimeError {
    fn from(error: std::io::Error) -> Self {
        Self::Storage { message: sanitize_storage_message(error.to_string()) }
    }
}

fn sanitize_storage_message(message: impl Display) -> String {
    let message = message.to_string();
    if message.len() > 512 {
        let mut end = 512;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &message[..end])
    } else {
        message
    }
}

/// Convenient result alias used by all public commands.
pub type Result<T> = std::result::Result<T, RuntimeError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_and_retry_hint_are_stable() {
        let error = RuntimeError::ModelTransport { message: "timeout".to_owned(), retryable: true };
        assert_eq!(error.code(), RuntimeErrorCode::ModelTransport);
        assert_eq!(error.stage(), RuntimeStage::Model);
        assert!(error.retryable());
    }

    #[test]
    fn object_lookup_does_not_expose_existence() {
        let error = RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Lifecycle };
        assert_eq!(error.to_string(), "object is not available");
    }

    #[test]
    fn error_codes_serialize_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&RuntimeErrorCode::ObjectNotAvailable).unwrap(),
            "\"object_not_available\""
        );
    }
}
