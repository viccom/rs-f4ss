use std::path::PathBuf;
use std::sync::{RwLock, TryLockError};
use std::time::Duration;

use crate::error::Error;
use crate::progress::ProgressState;
use crate::replace::{current_exe_resolved, download_and_replace, DownloadConfig, ReplaceResult};
use crate::restart;
use crate::source::{Release, Source};
use crate::version::{is_newer, validate_version};

/// A thread-safe logging function.
pub type LoggerFn = Box<dyn Fn(&str) + Send + Sync>;

/// Options for configuring an Updater.
#[derive(Default)]
pub struct UpdaterOptions {
    /// Custom logger function. Default: prints to stderr via tracing.
    pub logger: Option<LoggerFn>,
    /// Number of download retries (default: 3).
    pub retries: Option<u32>,
    /// Download timeout (default: 5min).
    pub timeout: Option<Duration>,
}

/// Checks for updates and applies them.
pub struct Updater {
    source: Box<dyn Source>,
    current: String,
    logger: LoggerFn,
    retries: u32,
    timeout: Duration,
    progress: ProgressState,
    /// Cached exe path before replacement, for restart().
    exe_path: RwLock<Option<PathBuf>>,
    /// Prevents concurrent updates.
    update_lock: std::sync::Mutex<()>,
}

impl Updater {
    pub fn new(
        source: impl Source + 'static,
        current_version: impl Into<String>,
        opts: UpdaterOptions,
    ) -> Self {
        Self {
            source: Box::new(source),
            current: current_version.into(),
            logger: opts
                .logger
                .unwrap_or_else(|| Box::new(|msg| tracing::info!("{}", msg))),
            retries: opts.retries.unwrap_or(3),
            timeout: opts.timeout.unwrap_or(Duration::from_secs(300)),
            progress: ProgressState::new(),
            exe_path: RwLock::new(None),
            update_lock: std::sync::Mutex::new(()),
        }
    }

    /// Query the source for a newer release.
    /// Returns `None` if the current version is already the latest.
    pub fn check(&self) -> Result<Option<Release>, Error> {
        validate_version(&self.current)?;
        let release = self.source.get_latest()?;
        // Reject malformed manifest versions up front: parse_version's
        // silent fallback to 0.0.0 would otherwise let an attacker with
        // a partially-controlled manifest trick us into treating a bad
        // version as newer.
        validate_version(&release.version)?;
        if !is_newer(&release.version, &self.current) {
            return Ok(None);
        }
        Ok(Some(release))
    }

    /// Download and install the update. Does NOT restart.
    /// Call `do_restart()` afterwards to restart the program.
    pub fn update(&self, release: &Release) -> Result<ReplaceResult, Error> {
        let _guard = match self.update_lock.try_lock() {
            Ok(g) => g,
            // Recover from poison. NOTE: `PoisonError::into_inner()`
            // returns the guard but does NOT clear the poison flag — the
            // mutex stays poisoned for the rest of its life (no stable
            // API to clear it). Every subsequent call therefore also
            // takes this branch; that works because we always apply the
            // same recovery, but a future caller that doesn't would
            // deadlock.
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(Error::UpdateInProgress),
        };
        self.update_inner(release)
    }

    /// Download, install, and restart the program in one call.
    pub fn update_and_restart(&self, release: &Release) -> Result<(), Error> {
        let _guard = match self.update_lock.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(Error::UpdateInProgress),
        };
        let _result = self.update_inner(release)?;
        (self.logger)("restarting ...");
        restart::restart(&self.cached_exe_path())?;
        Ok(())
    }

    /// Restart the program using the cached executable path from a prior `update()` call.
    pub fn do_restart(&self) -> Result<(), Error> {
        restart::restart(&self.cached_exe_path())
    }

    /// Returns the progress state for polling.
    pub fn progress(&self) -> ProgressState {
        self.progress.clone()
    }

    /// Returns the current version string.
    pub fn current_version(&self) -> &str {
        &self.current
    }

    fn cached_exe_path(&self) -> PathBuf {
        self.exe_path
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| current_exe_resolved().unwrap_or_default())
    }

    fn update_inner(&self, release: &Release) -> Result<ReplaceResult, Error> {
        let asset = release.asset_for_current_platform()?;

        // Cache exe path BEFORE replacement — on Linux /proc/self/exe may point
        // to a deleted .old file after binary replacement.
        let exe_path = current_exe_resolved()?;
        *self.exe_path.write().unwrap_or_else(|e| e.into_inner()) = Some(exe_path);

        self.progress.start();

        let config = DownloadConfig {
            max_retries: self.retries,
            retry_delay: Duration::from_secs(2),
            timeout: self.timeout,
            progress: Some(Box::new({
                let ps = self.progress.clone();
                move |downloaded, total| ps.set_progress(downloaded, total)
            })),
        };

        let result = match download_and_replace(asset, &config) {
            Ok(r) => r,
            Err(e) => {
                self.progress.set_error(&e.to_string());
                return Err(e);
            }
        };

        self.progress.set_done();
        (self.logger)(&format!("updated to {}", release.version));
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Minimal in-memory Source for tests.
    struct FixedSource(Arc<Release>);
    impl Source for FixedSource {
        fn get_latest(&self) -> Result<Release, Error> {
            Ok((*self.0).clone())
        }
    }

    fn make_release(version: &str) -> Arc<Release> {
        let mut assets = HashMap::new();
        assets.insert(
            Release::current_platform(),
            crate::source::Asset {
                url: "https://example.com/binary".into(),
                sha256: "0".repeat(64),
                size: 100,
            },
        );
        Arc::new(Release {
            version: version.into(),
            date: "2026-01-01".into(),
            assets,
        })
    }

    /// Regression: a manifest version that would otherwise be silently
    /// coerced to 0.0.0 by parse_version's fallback must be rejected.
    /// Otherwise an attacker-controlled manifest could cause a downgrade
    /// or an unintended update via the silent-prerelease fallback.
    #[test]
    fn check_rejects_malformed_manifest_version() {
        // "1.x" parses as 1.0.0 in the fallback path, but is not a
        // valid semver and must be rejected by validate_version.
        let src = FixedSource(make_release("1.x"));
        let updater = Updater::new(src, "1.0.0", UpdaterOptions::default());
        let err = updater.check().unwrap_err();
        assert!(matches!(err, Error::InvalidVersion(_)), "got: {}", err);
    }

    /// Regression: a manifest with a prerelease using underscore (which
    /// `semver::Prerelease::new` rejects) would silently become "1.0.0"
    /// via the empty-prerelease fallback — and then be considered NEWER
    /// than a non-prerelease current version. Must be rejected.
    #[test]
    fn check_rejects_invalid_prerelease() {
        let src = FixedSource(make_release("1.0.0-alpha_beta"));
        let updater = Updater::new(src, "1.0.0", UpdaterOptions::default());
        let err = updater.check().unwrap_err();
        assert!(matches!(err, Error::InvalidVersion(_)), "got: {}", err);
    }

    /// Happy path: a clean newer release is returned.
    #[test]
    fn check_returns_newer_release() {
        let src = FixedSource(make_release("1.1.0"));
        let updater = Updater::new(src, "1.0.0", UpdaterOptions::default());
        let release = updater.check().unwrap().expect("expected newer");
        assert_eq!(release.version, "1.1.0");
    }

    /// Same version: check() returns Ok(None).
    #[test]
    fn check_returns_none_when_current() {
        let src = FixedSource(make_release("1.0.0"));
        let updater = Updater::new(src, "1.0.0", UpdaterOptions::default());
        assert!(updater.check().unwrap().is_none());
    }
}
