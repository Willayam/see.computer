//! Every directory the app reads or writes, named once.

use std::path::PathBuf;

/// `~/Library/Application Support/see.computer`.
pub fn app_support() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("see.computer")
}

/// `~/Documents/see.computer`, where recordings, history, and the vocabulary
/// live, unless `SEE_COMPUTER_DOCUMENTS_DIR` names another folder for a check
/// that must not touch the real one.
pub fn documents() -> PathBuf {
    if let Some(path) = std::env::var_os("SEE_COMPUTER_DOCUMENTS_DIR") {
        return PathBuf::from(path);
    }
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("see.computer")
}

pub fn config() -> PathBuf {
    app_support().join("config.json")
}

pub fn instance_lock() -> PathBuf {
    app_support().join("instance.lock")
}

pub fn models() -> PathBuf {
    app_support().join("models/parakeet-tdt-0.6b-v3-onnx/int8")
}

pub fn ledger() -> PathBuf {
    app_support().join("ledger")
}

/// One line per dictation recording what Accessibility said about the frontmost
/// app. Written only while `SEE_COMPUTER_FOCUS_LOG` is set.
pub fn focus_log() -> PathBuf {
    app_support().join("focus-log.tsv")
}

/// One line per meeting transcription step.
pub fn meeting_log() -> PathBuf {
    app_support().join("meeting-log.tsv")
}

/// Where history goes when macOS denies Documents.
pub fn history_fallback() -> PathBuf {
    app_support().join("history")
}
