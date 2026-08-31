//! Every directory the app reads or writes, named once.

use std::path::PathBuf;

/// `~/Library/Application Support/see.computer`.
pub fn app_support() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("see.computer")
}

/// `~/Documents/see.computer`, where recordings, history, and the vocabulary live.
pub fn documents() -> PathBuf {
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

/// Where history goes when macOS denies Documents.
pub fn history_fallback() -> PathBuf {
    app_support().join("history")
}
