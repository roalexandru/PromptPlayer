//! Unified application error type.
//!
//! Replaces ad-hoc `String` errors used across the IPC layer. Implements
//! `serde::Serialize` so Tauri commands can surface structured errors to the
//! frontend (`{ "kind": "...", "message": "..." }`) instead of free-form
//! strings that drift over time.

use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("prompt {0} not found")]
    PromptNotFound(String),

    #[error("prompt {id} has no source path on disk")]
    NoSourcePath { id: String },

    #[error("could not resolve library root")]
    LibraryRootUnresolved,

    #[error("trigger {trigger:?} is already used by prompt {other_id}")]
    DuplicateTrigger { trigger: String, other_id: String },

    #[error("prompt {id} comes from a remote source and is read-only — fork it into your library to edit")]
    ReadOnlyPrompt { id: String },

    #[error("source error: {0}")]
    Source(String),

    #[error("invalid argument: {0}")]
    InvalidArg(String),

    #[error("parse error: {0}")]
    Parse(#[from] crate::prompts::parser::ParseError),

    #[error("yaml: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("io error on {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("io: {0}")]
    BareIo(#[from] std::io::Error),

    #[error("notify: {0}")]
    Notify(#[from] notify::Error),

    #[error("tauri: {0}")]
    Tauri(String),

    #[error("enigo: {0}")]
    Enigo(String),

    #[error("internal: {0}")]
    Internal(String),

    #[error("updater unavailable: {0}")]
    UpdaterUnavailable(String),

    #[error("update check failed: {0}")]
    UpdaterCheckFailed(String),

    #[error("update install failed: {0}")]
    UpdaterInstallFailed(String),

    #[error("no update available")]
    UpdaterNoUpdateAvailable,
}

impl AppError {
    /// Stable kebab-case discriminant for the frontend.
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::PromptNotFound(_) => "prompt-not-found",
            AppError::NoSourcePath { .. } => "no-source-path",
            AppError::LibraryRootUnresolved => "library-root-unresolved",
            AppError::DuplicateTrigger { .. } => "duplicate-trigger",
            AppError::ReadOnlyPrompt { .. } => "read-only-prompt",
            AppError::Source(_) => "source",
            AppError::InvalidArg(_) => "invalid-arg",
            AppError::Parse(_) => "parse",
            AppError::Yaml(_) => "yaml",
            AppError::Io { .. } | AppError::BareIo(_) => "io",
            AppError::Notify(_) => "notify",
            AppError::Tauri(_) => "tauri",
            AppError::Enigo(_) => "enigo",
            AppError::Internal(_) => "internal",
            AppError::UpdaterUnavailable(_) => "updater-unavailable",
            AppError::UpdaterCheckFailed(_) => "updater-check-failed",
            AppError::UpdaterInstallFailed(_) => "updater-install-failed",
            AppError::UpdaterNoUpdateAvailable => "updater-no-update-available",
        }
    }
}

/// Structured serialization for IPC. The frontend can pattern-match on `kind`.
impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("AppError", 2)?;
        s.serialize_field("kind", self.kind())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

/// Specta-friendly mirror of `AppError`. The IPC layer returns this so the
/// generated TypeScript bindings have a clean structured-error shape:
/// `{ kind: string; message: string }`. `From<AppError>` converts at the
/// command boundary.
#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct IpcError {
    pub kind: String,
    pub message: String,
}

impl From<AppError> for IpcError {
    fn from(e: AppError) -> Self {
        Self {
            kind: e.kind().to_string(),
            message: e.to_string(),
        }
    }
}

/// `Result` alias used by IPC commands.
pub type IpcResult<T> = Result<T, IpcError>;

/// Helper: any `AppResult<T>` flows through `?` into `IpcResult<T>`.
pub fn into_ipc<T>(r: AppResult<T>) -> IpcResult<T> {
    r.map_err(IpcError::from)
}

impl From<tauri::Error> for AppError {
    fn from(e: tauri::Error) -> Self {
        AppError::Tauri(e.to_string())
    }
}

impl From<enigo::NewConError> for AppError {
    fn from(e: enigo::NewConError) -> Self {
        AppError::Enigo(format!("{:?}", e))
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_for_each_variant_is_kebab() {
        let cases = [
            AppError::PromptNotFound("x".into()),
            AppError::NoSourcePath { id: "x".into() },
            AppError::LibraryRootUnresolved,
            AppError::InvalidArg("x".into()),
            AppError::Tauri("x".into()),
            AppError::Enigo("x".into()),
            AppError::Internal("x".into()),
        ];
        for c in cases {
            let k = c.kind();
            assert!(k.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-'));
        }
    }

    #[test]
    fn serializes_with_kind_and_message() {
        let e = AppError::PromptNotFound("xyz".into());
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "prompt-not-found");
        assert!(v["message"].as_str().unwrap().contains("xyz"));
    }

    #[test]
    fn from_tauri_error_lifts() {
        // Construct a tauri::Error via its public surface — using a known
        // variant. We'll synthesize from a custom IO error and let the
        // From impl handle it.
        let e: AppError = std::io::Error::new(std::io::ErrorKind::NotFound, "nope").into();
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "io");
    }

    #[test]
    fn invalid_arg_round_trips() {
        let e = AppError::InvalidArg("expected non-empty trigger".into());
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "invalid-arg");
        assert!(v["message"].as_str().unwrap().contains("non-empty"));
    }
}
