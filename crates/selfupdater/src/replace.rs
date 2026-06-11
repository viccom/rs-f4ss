use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::error::Error;
use crate::signature::{parse_public_key, parse_signature, verify_file};
use crate::source::{upgrade_to_https, Asset};

/// Result of a successful binary replacement.
pub struct ReplaceResult {
    /// Path to the executable that was replaced.
    pub exe_path: PathBuf,
    /// Path to the temp file that was used for the new binary (already deleted).
    pub tmp_path: PathBuf,
}

/// Configuration for the download operation.
pub struct DownloadConfig {
    /// Maximum number of retries on failure (default: 3).
    pub max_retries: u32,
    /// Delay between retries (default: 2s).
    pub retry_delay: Duration,
    /// Total download timeout (default: 5min).
    pub timeout: Duration,
    /// Optional progress callback: (downloaded, total).
    pub progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    /// Optional Minisign public key. When set, the asset MUST carry a
    /// signature and it MUST validate, or the update is rejected.
    /// Accepts either bare base64 or full `minisign.pub` format.
    pub public_key: Option<String>,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            retry_delay: Duration::from_secs(2),
            timeout: Duration::from_secs(300),
            progress: None,
            public_key: None,
        }
    }
}

/// Drop guard for the downloaded temp file. Ensures cleanup even if a
/// panic occurs between the download and the `self_replace` step.
struct TempFileGuard(PathBuf);
impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.0) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!("temp file cleanup: {}", e);
            }
        }
    }
}

/// Download, validate, and atomically replace the current binary.
///
/// Flow:
/// 1. Resolve current executable path
/// 2. Download asset to a temp file (same directory as exe, to avoid cross-fs rename)
/// 3. Validate SHA256 checksum
/// 4. If a public key is configured, verify the Minisign signature
/// 5. Set executable permission on Unix (AFTER all verification)
/// 6. Atomically replace using `self-replace`
/// 7. Drop guard removes the (now-moved) temp file
pub fn download_and_replace(
    asset: &Asset,
    config: &DownloadConfig,
) -> Result<ReplaceResult, Error> {
    let exe_path = current_exe_resolved()?;
    let tmp_path = download_with_retry(asset, config)?;
    let _guard = TempFileGuard(tmp_path.clone());

    validate_sha256(&tmp_path, &asset.sha256)?;
    verify_signature_if_configured(&tmp_path, asset, config)?;

    // Set executable permission only AFTER all verification, so an
    // unverified binary is never executable on disk.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))?;
    }

    self_replace::self_replace(&tmp_path).map_err(Error::Io)?;
    Ok(ReplaceResult {
        exe_path: exe_path.clone(),
        tmp_path: tmp_path.clone(),
    })
}

/// Verify the asset's Minisign signature when a public key is configured.
///
/// Fail-closed: if a public key is set, the asset MUST carry a signature and
/// it MUST validate. If no public key is set, this is a no-op (callers fall
/// back to SHA256-only integrity, which trusts the manifest channel).
fn verify_signature_if_configured(
    path: &Path,
    asset: &Asset,
    config: &DownloadConfig,
) -> Result<(), Error> {
    let pubkey_str = match config.public_key.as_deref() {
        Some(s) if !s.trim().is_empty() => s,
        _ => return Ok(()),
    };
    let sig_str = asset
        .signature
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or(Error::MissingSignature)?;

    let public_key = parse_public_key(pubkey_str)?;
    let signature = parse_signature(sig_str)?;
    verify_file(path, &signature, &public_key)
}

fn download_with_retry(asset: &Asset, config: &DownloadConfig) -> Result<PathBuf, Error> {
    let mut delay = config.retry_delay;
    let mut last_err: Option<String> = None;

    for attempt in 0..=config.max_retries {
        if attempt > 0 {
            std::thread::sleep(delay);
            delay = delay * 3 / 2;
        }
        match download_to_temp(asset, config) {
            Ok(path) => return Ok(path),
            Err(e) => {
                tracing::warn!("download attempt {} failed: {}", attempt + 1, e);
                last_err = Some(e.to_string());
            }
        }
    }

    Err(Error::DownloadFailed {
        retries: config.max_retries,
        reason: last_err.unwrap_or_else(|| "unknown error".into()),
    })
}

/// Hard cap on download size when the manifest doesn't specify one.
/// Prevents disk-fill DoS if a malicious server streams forever.
const MAX_DOWNLOAD_SIZE: u64 = 1 << 30; // 1 GB

fn download_to_temp(asset: &Asset, config: &DownloadConfig) -> Result<PathBuf, Error> {
    let client = reqwest::blocking::Client::builder()
        .timeout(config.timeout)
        .build()?;

    // Auto-upgrade http:// to https:// for non-localhost URLs (consistent with manifest URL)
    let url = upgrade_to_https(asset.url.clone());
    let resp = client.get(&url).send()?.error_for_status()?;
    let total = if asset.size > 0 {
        asset.size
    } else {
        resp.content_length().unwrap_or(0)
    };

    // Create temp file in the same directory as the executable
    // to avoid cross-filesystem rename issues.
    let exe = current_exe_resolved()?;
    let exe_dir = exe
        .parent()
        .ok_or_else(|| Error::NoParentDir(exe.clone()))?
        .to_path_buf();

    let tmp = tempfile::NamedTempFile::new_in(&exe_dir)?;
    let mut writer = std::io::BufWriter::new(tmp);
    let mut resp = resp;

    // Use the smaller of the manifest-declared size and the hard cap so
    // a missing size (0) still has a sane upper bound.
    let size_cap = if asset.size > 0 {
        asset.size.min(MAX_DOWNLOAD_SIZE)
    } else {
        MAX_DOWNLOAD_SIZE
    };

    let mut downloaded: u64 = 0;
    let mut last_report_bytes: u64 = 0;
    let mut last_report_time = std::time::Instant::now();

    let mut buf = vec![0u8; 32 * 1024];
    loop {
        let n = std::io::Read::read(&mut resp, &mut buf)?;
        if n == 0 {
            break;
        }

        // Per-chunk cap. Aborts BEFORE writing so a malicious server
        // cannot stream a multi-GB body and fill the disk.
        if downloaded + n as u64 > size_cap {
            return Err(Error::Download(format!(
                "download exceeds size cap: {} + {} > {}",
                downloaded, n, size_cap
            )));
        }

        std::io::Write::write_all(&mut writer, &buf[..n])?;
        downloaded += n as u64;

        // Report progress every 1MB or 500ms
        if let Some(ref cb) = config.progress {
            let now = std::time::Instant::now();
            if downloaded - last_report_bytes >= 1 << 20
                || now.duration_since(last_report_time) >= Duration::from_millis(500)
            {
                cb(downloaded, total);
                last_report_bytes = downloaded;
                last_report_time = now;
            }
        }
    }
    std::io::Write::flush(&mut writer)?;

    if let Some(ref cb) = config.progress {
        cb(downloaded, total);
    }

    // Validate download size if asset.size is known
    if asset.size > 0 && downloaded != asset.size {
        return Err(Error::Download(format!(
            "downloaded {} bytes, expected {}",
            downloaded, asset.size
        )));
    }

    // Extract the NamedTempFile from BufWriter
    let tmp = writer.into_inner().map_err(|e| Error::Io(e.into_error()))?;

    tracing::info!("downloaded {} bytes", downloaded);

    // Persist the temp file: convert to TempPath (survives drop) then keep it
    let path = tmp.into_temp_path();
    path.keep().map_err(|e| Error::Io(e.error))
}

fn validate_sha256(path: &Path, expected: &str) -> Result<(), Error> {
    if expected.is_empty() {
        return Err(Error::MissingSha256);
    }
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    let actual = format!("{:x}", hasher.finalize());
    // Case-insensitive compare: actual is always lowercase via {:x}, normalize expected.
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(Error::Sha256Mismatch {
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

/// Returns the resolved path of the current executable (following symlinks).
pub fn current_exe_resolved() -> Result<PathBuf, Error> {
    let exe = std::env::current_exe()?;
    let resolved = std::fs::canonicalize(&exe).unwrap_or(exe);
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_sha256_empty_rejects() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"test data").unwrap();
        let err = validate_sha256(tmp.path(), "").unwrap_err();
        assert!(matches!(err, Error::MissingSha256), "got: {}", err);
    }

    #[test]
    fn test_validate_sha256_case_insensitive() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"test data").unwrap();
        let lowercase = "916f0027a575074ce72a331777c3478d6513f786a591bd892da1a577bf2335f9";
        let uppercase = lowercase.to_uppercase();
        validate_sha256(tmp.path(), uppercase.as_str()).unwrap();
    }

    #[test]
    fn test_validate_sha256_match() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"test data").unwrap();
        let expected = "916f0027a575074ce72a331777c3478d6513f786a591bd892da1a577bf2335f9";
        validate_sha256(tmp.path(), expected).unwrap();
    }

    #[test]
    fn test_validate_sha256_mismatch() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"test data").unwrap();
        let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
        let err = validate_sha256(tmp.path(), wrong).unwrap_err();
        match err {
            Error::Sha256Mismatch { .. } => {}
            _ => panic!("expected Sha256Mismatch, got: {}", err),
        }
    }
}
