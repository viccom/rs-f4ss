mod error;
mod progress;
mod replace;
mod restart;
mod signature;
mod source;
mod updater;
mod version;

pub use error::Error;
pub use progress::{ProgressSnapshot, ProgressState};
pub use replace::{download_and_replace, DownloadConfig, ReplaceResult};
pub use restart::restart;
pub use signature::{parse_public_key, parse_signature, verify_file};
pub use source::{Asset, HttpSource, Release, Source};
pub use updater::{LoggerFn, Updater, UpdaterOptions};
pub use version::{compare_versions, is_newer, validate_version};

#[cfg(feature = "server")]
pub mod server;
