use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use crate::error::Error;

/// Platform-specific downloadable binary.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// Optional Minisign signature (full `.minisig` content, 4 lines).
    /// When the Updater is configured with a public key, this field MUST be
    /// present or the update is rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// An available update release.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Release {
    pub version: String,
    #[serde(default)]
    pub date: String,
    /// Platform key -> Asset. Keys are "os/arch", e.g. "linux/amd64".
    pub assets: HashMap<String, Asset>,
}

impl Release {
    /// Returns the platform string for the current OS/arch, e.g. "linux/amd64".
    /// Uses Go-compatible arch names for interoperability.
    pub fn current_platform() -> String {
        let arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            "x86" | "i686" => "386",
            "powerpc64" => "ppc64",
            "powerpc64le" => "ppc64le",
            "riscv64" | "riscv64gc" => "riscv64",
            "loongarch64" => "loong64",
            other => other,
        };
        format!("{}/{}", std::env::consts::OS, arch)
    }

    /// Returns the asset for a given platform key.
    pub fn asset_for_platform(&self, platform: &str) -> Result<&Asset, Error> {
        self.assets
            .get(platform)
            .ok_or_else(|| Error::NoAssetForPlatform {
                platform: platform.to_string(),
            })
    }

    /// Returns the asset for the current OS/arch.
    pub fn asset_for_current_platform(&self) -> Result<&Asset, Error> {
        self.asset_for_platform(&Self::current_platform())
    }
}

/// Trait for fetching release information from an update source.
pub trait Source: Send + Sync {
    fn get_latest(&self) -> Result<Release, Error>;
}

/// Fetches releases from a JSON HTTP endpoint.
///
/// The endpoint must return:
/// ```json
/// {
///   "version": "1.2.3",
///   "date": "2025-01-01T00:00:00Z",
///   "assets": {
///     "linux/amd64":   {"url": "...", "sha256": "...", "size": 12345},
///     "windows/amd64": {"url": "...", "sha256": "...", "size": 12345}
///   }
/// }
/// ```
pub struct HttpSource {
    url: String,
    client: reqwest::blocking::Client,
}

impl HttpSource {
    /// Create a new HttpSource with default settings (30s timeout).
    /// Auto-upgrades http:// to https:// for non-localhost URLs.
    pub fn new(url: impl Into<String>) -> Result<Self, Error> {
        Self::with_client(
            url,
            reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
        )
    }

    /// Create a new HttpSource with a custom reqwest client.
    pub fn with_client(
        url: impl Into<String>,
        client: reqwest::blocking::Client,
    ) -> Result<Self, Error> {
        let url = upgrade_to_https(url.into());
        Ok(Self { url, client })
    }
}

impl Source for HttpSource {
    fn get_latest(&self) -> Result<Release, Error> {
        const MAX_MANIFEST_SIZE: u64 = 1 << 20; // 1 MB
        let resp = self.client.get(&self.url).send()?.error_for_status()?;

        // Reject oversized manifests via Content-Length before buffering the body.
        // Servers that omit Content-Length (or lie about it) fall through to the
        // per-chunk check below.
        if let Some(len) = resp.content_length() {
            if len > MAX_MANIFEST_SIZE {
                return Err(Error::Download(format!(
                    "release manifest too large: {} bytes (max 1MB)",
                    len
                )));
            }
        }

        let bytes = read_bounded(resp, MAX_MANIFEST_SIZE)?;
        let release: Release = serde_json::from_slice(&bytes)?;
        if release.version.is_empty() {
            return Err(Error::InvalidVersion("release has no version".into()));
        }
        Ok(release)
    }
}

/// Read a response body into memory with a hard size cap applied per chunk,
/// so a server that omits or lies about `Content-Length` cannot allocate
/// unbounded RAM before the cap is checked.
fn read_bounded<R: Read>(mut reader: R, max_size: u64) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if (bytes.len() + n) as u64 > max_size {
            return Err(Error::Download(format!(
                "release manifest too large: exceeds {} bytes",
                max_size
            )));
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    Ok(bytes)
}

/// Auto-upgrade http:// to https:// for non-localhost URLs.
pub(crate) fn upgrade_to_https(url: String) -> String {
    // RFC 3986 §3.1: scheme is case-insensitive.
    if url.len() < 7 || !url[..7].eq_ignore_ascii_case("http://") {
        return url;
    }
    let rest = &url[7..]; // after "http://"
    let host = rest.split('/').next().unwrap_or(rest);
    let host_no_port = if host.starts_with('[') {
        // IPv6: [::1]:port — strip brackets
        host.trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or(host)
    } else {
        host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
    };
    if matches!(host_no_port, "127.0.0.1" | "::1" | "localhost") {
        return url;
    }
    format!("https://{}", rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_platform() {
        let p = Release::current_platform();
        assert!(!p.is_empty());
        assert!(p.contains('/'));
    }

    #[test]
    fn test_upgrade_to_https() {
        assert_eq!(
            upgrade_to_https("http://example.com/api".into()),
            "https://example.com/api"
        );
        assert_eq!(
            upgrade_to_https("https://example.com/api".into()),
            "https://example.com/api"
        );
        assert_eq!(
            upgrade_to_https("http://localhost:8080/api".into()),
            "http://localhost:8080/api"
        );
        assert_eq!(
            upgrade_to_https("http://127.0.0.1:8080/api".into()),
            "http://127.0.0.1:8080/api"
        );
        assert_eq!(
            upgrade_to_https("http://[::1]:8080/api".into()),
            "http://[::1]:8080/api"
        );
        // Case-insensitive scheme (RFC 3986 §3.1)
        assert_eq!(
            upgrade_to_https("HTTP://example.com/api".into()),
            "https://example.com/api"
        );
        assert_eq!(
            upgrade_to_https("Http://Example.COM/api".into()),
            "https://Example.COM/api"
        );
    }

    #[test]
    fn test_release_deserialize() {
        let json = r#"{
            "version": "1.2.3",
            "date": "2025-01-01",
            "assets": {
                "linux/amd64": {"url": "http://example.com/bin", "sha256": "abc123", "size": 100}
            }
        }"#;
        let release: Release = serde_json::from_str(json).unwrap();
        assert_eq!(release.version, "1.2.3");
        let asset = release.asset_for_platform("linux/amd64").unwrap();
        assert_eq!(asset.sha256, "abc123");
        assert_eq!(asset.size, 100);
    }

    #[test]
    fn test_asset_not_found() {
        let json = r#"{"version": "1.0.0", "assets": {}}"#;
        let release: Release = serde_json::from_str(json).unwrap();
        assert!(release.asset_for_platform("linux/amd64").is_err());
    }

    #[test]
    fn read_bounded_caps_size() {
        let data = vec![0u8; 2000];
        let result = read_bounded(&data[..], 1000);
        assert!(matches!(result, Err(Error::Download(_))));
    }

    #[test]
    fn read_bounded_allows_under_cap() {
        let data = [0u8; 100];
        let result = read_bounded(&data[..], 1000).unwrap();
        assert_eq!(result.len(), 100);
    }
}
