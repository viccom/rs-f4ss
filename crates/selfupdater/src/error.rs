use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no asset for platform {platform}")]
    NoAssetForPlatform { platform: String },

    #[error("invalid version: {0}")]
    InvalidVersion(String),

    #[error("sha256 mismatch: expected {expected}, got {actual}")]
    Sha256Mismatch { expected: String, actual: String },

    #[error("missing sha256 checksum — integrity verification required")]
    MissingSha256,

    #[error("download failed after {retries} retries: {reason}")]
    DownloadFailed { retries: u32, reason: String },

    #[error("download: {0}")]
    Download(String),

    #[error("http {status}: {url}")]
    HttpError { status: u16, url: String },

    #[error("update already in progress")]
    UpdateInProgress,

    #[error("no cached executable path — call update() first")]
    NoCachedExePath,

    #[error("executable has no parent directory: {0}")]
    NoParentDir(PathBuf),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
