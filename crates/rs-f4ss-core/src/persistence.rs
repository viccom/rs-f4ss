use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::manager::MountEntry;

#[cfg(feature = "serve")]
use crate::share_manager::ShareConfig;

#[cfg(any(feature = "api", feature = "serve"))]
use sha2::{Digest, Sha256};

const CONFIG_FILE: &str = "config.json";

// ---------------------------------------------------------------------------
// Auth config (stored in config.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub username: String,
    pub password_hash: String,
}

#[cfg(any(feature = "api", feature = "serve"))]
impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            username: "admin".to_string(),
            password_hash: hash_password("admin"),
        }
    }
}

#[cfg(any(feature = "api", feature = "serve"))]
pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Salted password hash, self-describing format: `s256$<salt_hex>$<hash_hex>`.
///
/// The salt is per-call random, so the same password hashes differently every
/// time — verification goes through [`verify_password`], never equality.
#[cfg(any(feature = "api", feature = "serve"))]
pub fn hash_password(password: &str) -> String {
    const ITERATIONS: u32 = 10_000;

    let mut salt = [0u8; 16];
    rand::fill(&mut salt);

    let mut hash = salt.to_vec();
    hash.extend_from_slice(password.as_bytes());
    for _ in 0..ITERATIONS {
        let mut hasher = Sha256::new();
        hasher.update(&hash);
        hash = hasher.finalize().to_vec();
    }

    format!("s256${}${}", hex_encode(&salt), hex_encode(&hash))
}

/// Constant-time verification of a password against a stored hash.
///
/// Accepts both the salted `s256$…` format and legacy bare SHA-256 hex (so a
/// config written by an older build still verifies; `hash_password` output
/// written back on the next successful password change migrates it).
#[cfg(any(feature = "api", feature = "serve"))]
pub fn verify_password(password: &str, stored: &str) -> bool {
    if let Some(rest) = stored.strip_prefix("s256$") {
        let Some((salt_hex, hash_hex)) = rest.split_once('$') else {
            return false;
        };
        let Ok(salt) = hex_decode(salt_hex) else {
            return false;
        };
        let mut hash = salt.clone();
        hash.extend_from_slice(password.as_bytes());
        for _ in 0..10_000 {
            let mut hasher = Sha256::new();
            hasher.update(&hash);
            hash = hasher.finalize().to_vec();
        }
        return constant_time_eq(&hash, &hex_decode(hash_hex).unwrap_or_default());
    }
    // Legacy unsalted SHA-256.
    constant_time_eq(sha256_hex(password).as_bytes(), stored.as_bytes())
}

#[cfg(any(feature = "api", feature = "serve"))]
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(any(feature = "api", feature = "serve"))]
fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    if s.len() % 2 != 0 {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

/// Constant-time byte comparison; always walks the full input.
#[cfg(any(feature = "api", feature = "serve"))]
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let len = a.len().max(b.len());
    let mut result = a.len() ^ b.len();
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        result |= (x ^ y) as usize;
    }
    result == 0
}

/// Whether `hash` still carries the default "admin" credential.
#[cfg(any(feature = "api", feature = "serve"))]
pub fn is_default_auth(auth: &AuthConfig) -> bool {
    auth.username == "admin" && verify_password("admin", &auth.password_hash)
}

// ---------------------------------------------------------------------------
// Public paths
// ---------------------------------------------------------------------------

pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rs-f4ss"))
}

pub fn default_config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join(CONFIG_FILE))
}

// ---------------------------------------------------------------------------
// Unified store: { "mounts": [...], "shares": [...] }
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct AppStore {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth: Option<AuthConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    listen: Option<String>,
    #[serde(default)]
    mounts: Vec<MountEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    shares: Vec<ShareConfigSer>,
}

// ShareConfigSer is always defined (used in AppStore),
// but only contains real fields when "serve" feature is active.
#[derive(Serialize, Deserialize, Default, Clone)]
struct ShareConfigSer {
    #[serde(default)]
    id: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    addr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pass: Option<String>,
    #[serde(default)]
    read_only: bool,
}

/// Normalize empty-string user/pass to `None`: an empty credential string must
/// not silently enable Basic auth with empty credentials.
#[cfg(feature = "serve")]
pub(crate) fn normalize_share_auth(
    id: &str,
    user: Option<String>,
    pass: Option<String>,
) -> (Option<String>, Option<String>) {
    let had_empty =
        user.as_deref().is_some_and(str::is_empty) || pass.as_deref().is_some_and(str::is_empty);
    let user = user.filter(|s| !s.is_empty());
    let pass = pass.filter(|s| !s.is_empty());
    if had_empty {
        warn!("Share {id}: empty user/pass treated as no authentication");
    }
    (user, pass)
}

#[cfg(feature = "serve")]
impl From<ShareConfigSer> for ShareConfig {
    fn from(s: ShareConfigSer) -> Self {
        let (user, pass) = normalize_share_auth(&s.id, s.user, s.pass);
        ShareConfig {
            id: s.id,
            path: s.path,
            addr: s.addr,
            user,
            pass,
            read_only: s.read_only,
        }
    }
}

#[cfg(feature = "serve")]
impl From<ShareConfig> for ShareConfigSer {
    fn from(s: ShareConfig) -> Self {
        ShareConfigSer {
            id: s.id,
            path: s.path,
            addr: s.addr,
            user: s.user,
            pass: s.pass,
            read_only: s.read_only,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal read/write
// ---------------------------------------------------------------------------

/// Global lock to serialize read-modify-write cycles on the config file.
fn store_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn read_store(path: &Path) -> AppStore {
    let data = match fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return AppStore::default(),
        Err(e) => {
            warn!("Cannot read config {}: {e}", path.display());
            return AppStore::default();
        }
    };

    // Try unified format: { "mounts": [...], "shares": [...] }
    if let Ok(store) = serde_json::from_str::<AppStore>(&data) {
        return store;
    }

    // Backward compat: old format was a plain array of MountEntry
    if let Ok(mounts) = serde_json::from_str::<Vec<MountEntry>>(&data) {
        return AppStore {
            auth: None,
            listen: None,
            mounts,
            shares: Vec::new(),
        };
    }

    warn!("Cannot parse config {}", path.display());
    AppStore::default()
}

fn write_store(store: &AppStore, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return Err(format!(
                "Cannot create config dir {}: {e}",
                parent.display()
            ));
        }
    }

    let json =
        serde_json::to_string_pretty(store).map_err(|e| format!("Cannot serialize config: {e}"))?;

    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, &json)
        .map_err(|e| format!("Cannot write config {}: {e}", tmp_path.display()))?;
    fs::rename(&tmp_path, path).map_err(|e| format!("Cannot rename config: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            warn!("Cannot set config permissions: {e}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Mount persistence
// ---------------------------------------------------------------------------

pub fn save(entries: &DashMap<String, MountEntry>, path: &Path) {
    let _guard = store_lock().lock().unwrap();
    let mut store = read_store(path);
    store.mounts = entries.iter().map(|r| r.value().clone()).collect();
    if let Err(e) = write_store(&store, path) {
        warn!("{e}");
    }
}

pub fn load(path: &Path) -> Vec<MountEntry> {
    read_store(path).mounts
}

// ---------------------------------------------------------------------------
// Auth persistence
// ---------------------------------------------------------------------------

#[cfg(feature = "api")]
pub fn load_auth(path: &Path) -> AuthConfig {
    match read_store(path).auth {
        Some(auth) => auth,
        None => {
            tracing::warn!("No auth config found in {}, using default credentials (admin:admin). Please change the password.", path.display());
            AuthConfig::default()
        }
    }
}

#[cfg(feature = "api")]
pub fn save_auth(auth: &AuthConfig, path: &Path) -> Result<(), String> {
    let _guard = store_lock().lock().unwrap();
    let mut store = read_store(path);
    store.auth = Some(auth.clone());
    write_store(&store, path)
}

/// Optional top-level `listen` address for `serve`, read from the config file.
#[cfg(feature = "api")]
pub fn load_listen(path: &Path) -> Option<String> {
    read_store(path).listen
}

// ---------------------------------------------------------------------------
// Share persistence
// ---------------------------------------------------------------------------

#[cfg(feature = "serve")]
pub fn save_shares(entries: &DashMap<String, ShareConfig>, path: &Path) {
    let _guard = store_lock().lock().unwrap();
    let mut store = read_store(path);
    store.shares = entries
        .iter()
        .map(|r| ShareConfigSer::from(r.value().clone()))
        .collect();
    if let Err(e) = write_store(&store, path) {
        warn!("{e}");
    }
}

#[cfg(feature = "serve")]
pub fn load_shares(path: &Path) -> Vec<ShareConfig> {
    read_store(path)
        .shares
        .into_iter()
        .map(ShareConfig::from)
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_entry(id: &str) -> MountEntry {
        MountEntry {
            id: id.to_string(),
            url: "http://localhost:9000".to_string(),
            mountpoint: PathBuf::from(format!("/mnt/{id}")),
            read_only: false,
            username: Some("admin".to_string()),
            password: Some("secret".to_string()),
            cache_ttl_secs: 5,
            cache_size: 256,
        }
    }

    #[test]
    fn test_save_and_load() {
        let dir = std::env::temp_dir().join("rs-f4ss-persist-test-1");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.json");

        let entries = DashMap::new();
        entries.insert("m1".to_string(), test_entry("m1"));
        entries.insert("m2".to_string(), test_entry("m2"));

        save(&entries, &path);
        let loaded = load(&path);

        assert_eq!(loaded.len(), 2);
        let m1 = loaded.iter().find(|e| e.id == "m1").unwrap();
        assert_eq!(m1.url, "http://localhost:9000");
        assert_eq!(m1.password, Some("secret".to_string()));
    }

    #[test]
    fn test_load_missing_file() {
        let loaded = load(Path::new("/tmp/rs-f4ss-nonexistent-config.json"));
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_load_corrupt_file() {
        let dir = std::env::temp_dir().join("rs-f4ss-corrupt-test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "not valid json {{{").unwrap();

        let loaded = load(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_backward_compat_old_format() {
        let dir = std::env::temp_dir().join("rs-f4ss-old-format-test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.json");
        fs::create_dir_all(&dir).unwrap();

        // Write old format: plain array of MountEntry
        let old_entries = vec![test_entry("old1")];
        fs::write(&path, serde_json::to_string_pretty(&old_entries).unwrap()).unwrap();

        let loaded = load(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "old1");
    }

    #[test]
    fn test_new_format_preserves_shares() {
        let dir = std::env::temp_dir().join("rs-f4ss-unified-test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.json");

        // Save mounts
        let mounts = DashMap::new();
        mounts.insert("m1".to_string(), test_entry("m1"));
        save(&mounts, &path);

        // Add shares via direct store manipulation
        let share_cfg = ShareConfigSer {
            id: "s1".to_string(),
            path: "/tmp".to_string(),
            addr: "0.0.0.0:9001".to_string(),
            user: Some("admin".to_string()),
            pass: None,
            read_only: true,
        };
        let mut store = read_store(&path);
        store.shares = vec![share_cfg];
        write_store(&store, &path).unwrap();

        // Reload mounts — should still be there
        let loaded_mounts = load(&path);
        assert_eq!(loaded_mounts.len(), 1);
        assert_eq!(loaded_mounts[0].id, "m1");

        // Check raw JSON has both keys
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"mounts\""));
        assert!(raw.contains("\"shares\""));
    }

    #[test]
    fn test_password_none_not_in_json() {
        let dir = std::env::temp_dir().join("rs-f4ss-no-pw-test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.json");

        let entries = DashMap::new();
        let mut entry = test_entry("nopw");
        entry.password = None;
        entries.insert("nopw".to_string(), entry);

        save(&entries, &path);

        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("password"));

        let loaded = load(&path);
        assert_eq!(loaded[0].password, None);
    }

    #[test]
    #[cfg(feature = "serve")]
    fn test_share_round_trip() {
        let dir = std::env::temp_dir().join("rs-f4ss-share-rt-test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.json");

        let entries = DashMap::new();
        entries.insert(
            "s1".to_string(),
            ShareConfig {
                id: "s1".to_string(),
                path: "/tmp".to_string(),
                addr: "0.0.0.0:9001".to_string(),
                user: Some("admin".to_string()),
                pass: Some("secret".to_string()),
                read_only: true,
            },
        );
        save_shares(&entries, &path);

        let loaded = load_shares(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "s1");
        assert_eq!(loaded[0].path, "/tmp");
        assert_eq!(loaded[0].pass, Some("secret".to_string()));

        // Mounts should be empty
        let loaded_mounts = load(&path);
        assert!(loaded_mounts.is_empty());
    }

    #[test]
    #[cfg(feature = "serve")]
    fn test_both_persist_together() {
        let dir = std::env::temp_dir().join("rs-f4ss-together-test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.json");

        // Save mounts
        let mounts = DashMap::new();
        mounts.insert("m1".to_string(), test_entry("m1"));
        save(&mounts, &path);

        // Save shares — mounts must survive
        let shares = DashMap::new();
        shares.insert(
            "s1".to_string(),
            ShareConfig {
                id: "s1".to_string(),
                path: "/tmp".to_string(),
                addr: "0.0.0.0:9001".to_string(),
                user: None,
                pass: None,
                read_only: false,
            },
        );
        save_shares(&shares, &path);

        // Both must be present
        assert_eq!(load(&path).len(), 1);
        assert_eq!(load_shares(&path).len(), 1);

        // Update mount — shares must survive
        mounts.insert("m2".to_string(), test_entry("m2"));
        save(&mounts, &path);
        assert_eq!(load(&path).len(), 2);
        assert_eq!(load_shares(&path).len(), 1);
    }

    #[test]
    #[cfg(all(feature = "api", feature = "serve"))]
    fn test_load_listen_and_empty_share_auth_normalized() {
        let dir = std::env::temp_dir().join("rs-f4ss-listen-norm-test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.json");
        fs::create_dir_all(&dir).unwrap();

        // Same structure AppStore serializes, written by hand.
        fs::write(
            &path,
            r#"{
  "listen": "127.0.0.1:9999",
  "mounts": [],
  "shares": [
    { "id": "s1", "path": "/tmp", "addr": "0.0.0.0:9001", "user": "", "pass": "", "read_only": false }
  ]
}"#,
        )
        .unwrap();

        assert_eq!(load_listen(&path), Some("127.0.0.1:9999".to_string()));

        let shares = load_shares(&path);
        assert_eq!(shares.len(), 1);
        assert_eq!(shares[0].id, "s1");
        assert_eq!(shares[0].user, None, "empty user must normalize to None");
        assert_eq!(shares[0].pass, None, "empty pass must normalize to None");
    }

    #[test]
    #[cfg(any(feature = "api", feature = "serve"))]
    fn test_hash_password_is_salted_and_verifiable() {
        let h1 = hash_password("hunter2");
        let h2 = hash_password("hunter2");
        // Same password, two different salts.
        assert_ne!(h1, h2);
        assert!(h1.starts_with("s256$"), "format: {h1}");
        assert!(verify_password("hunter2", &h1));
        assert!(!verify_password("hunter3", &h1));
        assert!(!verify_password("", &h1));
    }

    #[test]
    #[cfg(any(feature = "api", feature = "serve"))]
    fn test_verify_password_accepts_legacy_unsalted_hash() {
        let legacy = sha256_hex("admin");
        assert!(verify_password("admin", &legacy));
        assert!(!verify_password("root", &legacy));
        // Garbage input must not panic.
        assert!(!verify_password("admin", "s256$"));
        assert!(!verify_password("admin", "s256$zz$zz"));
    }

    #[test]
    #[cfg(any(feature = "api", feature = "serve"))]
    fn test_default_auth_uses_salted_hash_and_is_detected() {
        let auth = AuthConfig::default();
        assert_ne!(auth.password_hash, sha256_hex("admin"));
        assert!(is_default_auth(&auth));
        let changed = AuthConfig {
            username: "admin".to_string(),
            password_hash: hash_password("new-pass"),
        };
        assert!(!is_default_auth(&changed));
    }

    #[test]
    #[cfg(any(feature = "api", feature = "serve"))]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }
}
