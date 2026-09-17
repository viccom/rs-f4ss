mod error;
mod progress;
mod replace;
mod restart;
mod source;
mod updater;
mod version;

pub use error::Error;
pub use progress::{ProgressSnapshot, ProgressState};
pub use replace::{download_and_replace, DownloadConfig, ReplaceResult};
pub use restart::restart;
pub use source::{Asset, HttpSource, Release, Source};
pub use updater::{LoggerFn, Updater, UpdaterOptions};
pub use version::{compare_versions, is_newer, validate_version};
