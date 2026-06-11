//! Self-updater wrapper built on top of the [`selfupdater`] crate.
//!
//! Exposes a `SelfUpdater` that owns the underlying `selfupdater::Updater`
//! so the rest of the codebase does not need to depend on the third-party
//! crate directly. The CLI and the REST API both go through this wrapper.
//!
//! Wire format (`latest.json`):
//! ```json
//! {
//!   "version": "0.3.0",
//!   "date":    "2026-06-11T00:00:00Z",
//!   "assets": {
//!     "linux/amd64":   {"url": "...", "sha256": "...", "size": 7700000},
//!     "windows/amd64": {"url": "...", "sha256": "...", "size": 9500000}
//!   }
//! }
//! ```
//! Optional Ed25519/minisign signature per asset when a public key is
//! configured (defends against a compromised manifest host).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use selfupdater::{HttpSource, Updater, UpdaterOptions};
use serde::Serialize;

pub use selfupdater::{Error, ProgressSnapshot, ProgressState, Release, Source};

/// Default manifest URL — points at the latest GitHub release.
pub const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/viccom/rs-f4ss/releases/latest/download/latest.json";

/// Environment override for the manifest URL.
pub const ENV_MANIFEST_URL: &str = "RS_F4SS_UPDATE_URL";

/// Environment override for the minisign public key (enables signature
/// verification when set).
pub const ENV_PUBLIC_KEY: &str = "RS_F4SS_UPDATE_PUBKEY";

/// Default manifest fetch timeout.
const DEFAULT_MANIFEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Configuration for [`SelfUpdater`].
#[derive(Debug, Clone)]
pub struct SelfUpdateConfig {
    /// URL of the JSON release manifest. Defaults to the GitHub releases
    /// endpoint, override via env `RS_F4SS_UPDATE_URL` or constructor.
    pub manifest_url: String,
    /// Optional minisign public key (full `minisign.pub` content or the
    /// bare base64 key string). When set, every asset MUST carry a
    /// signature that validates against this key.
    pub public_key: Option<String>,
    /// Download timeout for both the manifest fetch and the binary
    /// download (default: manifest 30s, binary 5min).
    pub timeout: Option<Duration>,
    /// Number of retries on transient download failures (default: 3).
    pub retries: Option<u32>,
}

impl SelfUpdateConfig {
    /// Build a config from environment variables, falling back to
    /// [`DEFAULT_MANIFEST_URL`].
    pub fn from_env() -> Self {
        let manifest_url =
            std::env::var(ENV_MANIFEST_URL).unwrap_or_else(|_| DEFAULT_MANIFEST_URL.to_string());
        let public_key = std::env::var(ENV_PUBLIC_KEY)
            .ok()
            .filter(|s| !s.trim().is_empty());
        Self {
            manifest_url,
            public_key,
            timeout: None,
            retries: None,
        }
    }
}

impl Default for SelfUpdateConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

/// Thin wrapper around `selfupdater::Updater`. Cheap to clone (the inner
/// `Updater` is wrapped in an `Arc`).
#[derive(Clone)]
pub struct SelfUpdater {
    inner: Arc<Updater>,
    config: SelfUpdateConfig,
    /// Set by `apply()` after the binary on disk is replaced; consumed
    /// (cleared) by `do_restart()`. Lets the REST layer reject restart
    /// calls that arrive before a successful apply.
    pending: Arc<AtomicBool>,
}

impl SelfUpdater {
    /// Build a self-updater for the current binary, using the supplied
    /// configuration. `current_version` should be `env!("CARGO_PKG_VERSION")`
    /// in production.
    pub fn new<S: Into<String>>(
        current_version: S,
        config: SelfUpdateConfig,
    ) -> Result<Self, Error> {
        // Honour the configured timeout for BOTH the manifest fetch
        // and the binary download. Previously the manifest fetch was
        // hard-coded to 30s inside `HttpSource::new`, which surprised
        // operators who shortened `timeout` for slow networks.
        let timeout = config.timeout.unwrap_or(DEFAULT_MANIFEST_TIMEOUT);
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(Error::from)?;
        let source = HttpSource::with_client(config.manifest_url.clone(), client)?;
        let opts = UpdaterOptions {
            public_key: config.public_key.clone(),
            retries: config.retries,
            timeout: config.timeout,
            ..Default::default()
        };
        let inner = Updater::new(source, current_version.into(), opts);
        Ok(Self {
            inner: Arc::new(inner),
            config,
            pending: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Current version string passed to the underlying updater.
    pub fn current_version(&self) -> &str {
        self.inner.current_version()
    }

    /// Effective manifest URL.
    pub fn manifest_url(&self) -> &str {
        &self.config.manifest_url
    }

    /// Whether a minisign public key is configured.
    pub fn has_public_key(&self) -> bool {
        self.config.public_key.is_some()
    }

    /// Returns the current platform key in `os/arch` form (e.g. `linux/amd64`).
    pub fn platform() -> String {
        selfupdater::Release::current_platform()
    }

    /// Path to the running executable (best-effort, follows symlinks).
    pub fn current_exe_path() -> Result<PathBuf, Error> {
        let exe = std::env::current_exe()?;
        let resolved = std::fs::canonicalize(&exe).unwrap_or(exe);
        Ok(resolved)
    }

    /// Query the manifest for a newer release. Returns `None` when the
    /// running version is already the latest.
    pub fn check(&self) -> Result<Option<Release>, Error> {
        self.inner.check()
    }

    /// Download, validate, and atomically replace the running binary.
    /// Does NOT restart the process — call [`Self::apply_and_restart`] or
    /// [`Self::do_restart`] to restart.
    pub fn apply(&self, release: &Release) -> Result<(), Error> {
        self.inner.update(release).map(|_| ())?;
        self.pending.store(true, Ordering::Release);
        Ok(())
    }

    /// Download, validate, replace, and immediately restart the process
    /// in place (Unix: `execv`, preserves PID; Windows: spawns new
    /// process with 2s health check).
    pub fn apply_and_restart(&self, release: &Release) -> Result<(), Error> {
        self.inner.update(release)?;
        // Mark pending first so the call is observable, but on success
        // we execv and the flag is dropped with the process.
        self.pending.store(true, Ordering::Release);
        self.inner.do_restart()
    }

    /// Restart the process using the executable path cached during a
    /// prior successful [`Self::apply`] call. Returns
    /// `Error::NoCachedExePath` if there is no pending update.
    pub fn do_restart(&self) -> Result<(), Error> {
        if !self.pending.swap(false, Ordering::AcqRel) {
            return Err(Error::NoCachedExePath);
        }
        self.inner.do_restart()
    }

    /// Snapshot of the in-flight download progress.
    pub fn progress_snapshot(&self) -> ProgressSnapshot {
        self.inner.progress().snapshot()
    }

    /// True if `apply()` succeeded and `do_restart()` has not yet been
    /// called. The REST layer uses this to reject restart-without-apply.
    pub fn has_pending_update(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
}

/// JSON-serializable status view returned by `GET /api/update/version`.
#[derive(Debug, Serialize)]
pub struct UpdateInfo {
    pub current_version: String,
    pub manifest_url: String,
    pub platform: String,
    pub exe_path: Option<String>,
    pub public_key_configured: bool,
    /// True after a successful `apply()` and before `do_restart()`.
    /// Lets the Web UI surface a "Restart to apply" button only when
    /// the binary on disk is newer than what's running.
    pub pending_update: bool,
}

impl UpdateInfo {
    pub fn from_updater(updater: &SelfUpdater) -> Self {
        Self {
            current_version: updater.current_version().to_string(),
            manifest_url: updater.manifest_url().to_string(),
            platform: SelfUpdater::platform(),
            exe_path: SelfUpdater::current_exe_path()
                .ok()
                .map(|p| p.display().to_string()),
            public_key_configured: updater.has_public_key(),
            pending_update: updater.has_pending_update(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selfupdater::Asset;
    use std::collections::HashMap;

    fn make_release(version: &str) -> Release {
        let mut assets = HashMap::new();
        assets.insert(
            Release::current_platform(),
            Asset {
                url: "https://example.com/binary".into(),
                sha256: "0".repeat(64),
                size: 100,
                signature: None,
            },
        );
        Release {
            version: version.into(),
            date: "2026-01-01".into(),
            assets,
        }
    }

    #[test]
    fn default_manifest_url_targets_releases_endpoint() {
        assert!(DEFAULT_MANIFEST_URL.contains("rs-f4ss"));
        assert!(DEFAULT_MANIFEST_URL.ends_with("latest.json"));
    }

    #[test]
    fn release_roundtrip_serde() {
        let r = make_release("1.2.3");
        let json = serde_json::to_string(&r).unwrap();
        let back: Release = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, "1.2.3");
        assert!(back.assets.contains_key(&Release::current_platform()));
    }

    #[test]
    fn platform_string_is_non_empty() {
        let p = SelfUpdater::platform();
        assert!(p.contains('/'));
    }

    #[test]
    fn update_info_serializes_expected_fields() {
        let info = UpdateInfo {
            current_version: "1.2.3".into(),
            manifest_url: "https://example.com/latest.json".into(),
            platform: "linux/amd64".into(),
            exe_path: Some("/usr/bin/rs-f4ss".into()),
            public_key_configured: false,
            pending_update: true,
        };
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["current_version"], "1.2.3");
        assert_eq!(v["platform"], "linux/amd64");
        assert_eq!(v["public_key_configured"], false);
        assert_eq!(v["pending_update"], true);
    }

    /// `do_restart()` without a prior `apply()` must return
    /// `NoCachedExePath` so the REST layer can surface a 400.
    #[test]
    fn do_restart_without_apply_errors() {
        // We can't construct a `SelfUpdater` with an unreachable URL
        // without paying a network round-trip on some platforms, so
        // check the underlying error variant by going through the
        // public surface: build an updater, then call do_restart().
        // (The HTTP source does not fetch on construction, so this is
        // purely in-process.)
        let cfg = SelfUpdateConfig {
            manifest_url: "http://127.0.0.1:1/latest.json".into(),
            public_key: None,
            timeout: Some(Duration::from_millis(100)),
            retries: Some(0),
        };
        let updater = SelfUpdater::new("1.0.0", cfg).unwrap();
        assert!(!updater.has_pending_update());
        let err = updater.do_restart().unwrap_err();
        assert!(matches!(err, Error::NoCachedExePath), "got: {err}");
    }

    /// Integration test: spin up a loopback HTTP server, point the
    /// updater at it, and verify `check()` sees the manifest we served.
    #[test]
    fn check_against_loopback_server() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        // Build a manifest newer than the running version.
        let manifest = make_release("99.0.0");
        let body = serde_json::to_vec(&manifest).unwrap();

        // Bind a loopback listener; let the OS pick the port.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        // Single-shot HTTP/1.1 server: reply to the first request and
        // then exit. Selfupdater's HttpSource uses a single GET, so
        // a one-shot handler is enough.
        let server = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });

        let cfg = SelfUpdateConfig {
            manifest_url: format!("http://{addr}/latest.json"),
            public_key: None,
            timeout: Some(Duration::from_secs(5)),
            retries: Some(0),
        };
        let updater = SelfUpdater::new("0.0.1", cfg).expect("updater");
        let release = updater.check().expect("check").expect("newer release");
        assert_eq!(release.version, "99.0.0");

        let _ = server.join();
    }
}
