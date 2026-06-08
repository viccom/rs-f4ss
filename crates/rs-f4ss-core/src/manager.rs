//! MountManager — dynamic mount configuration and lifecycle management.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::mount::UnmountCallback;
use std::sync::Arc;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use crate::backend::StorageBackend;
use crate::mount::{MountConfig, MountEngine};
use crate::persistence;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountEntry {
    pub id: String,
    pub url: String,
    pub mountpoint: PathBuf,
    pub read_only: bool,
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_secs: u64,
    #[serde(default = "default_cache_size")]
    pub cache_size: usize,
}

pub(crate) fn default_cache_ttl() -> u64 {
    60
}
pub(crate) fn default_cache_size() -> usize {
    256
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MountState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountInfo {
    pub id: String,
    pub url: String,
    pub protocol: String,
    pub mountpoint: String,
    pub state: MountState,
    pub read_only: bool,
    pub cache_ttl_secs: u64,
    pub cache_size: usize,
}

// ---------------------------------------------------------------------------
// Internal handle for a running mount
// ---------------------------------------------------------------------------

struct MountHandle {
    state: Arc<std::sync::Mutex<MountState>>,
    cancel: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    unmount_slot: Arc<std::sync::Mutex<Option<UnmountCallback>>>,
}

impl MountHandle {
    fn state(&self) -> MountState {
        self.state.lock().unwrap().clone()
    }
}

// ---------------------------------------------------------------------------
// MountManager
// ---------------------------------------------------------------------------

pub struct MountManager {
    entries: DashMap<String, MountEntry>,
    handles: DashMap<String, MountHandle>,
    persist_path: Option<PathBuf>,
}

impl Default for MountManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MountManager {
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            handles: DashMap::new(),
            persist_path: None,
        }
    }

    pub fn new_with_persistence(path: PathBuf) -> Self {
        Self {
            entries: DashMap::new(),
            handles: DashMap::new(),
            persist_path: Some(path),
        }
    }

    pub fn restore_entries(&self) {
        let path = match &self.persist_path {
            Some(p) => p.clone(),
            None => return,
        };
        let entries = persistence::load(&path);
        let count = entries.len();
        for entry in entries {
            let id = entry.id.clone();
            use dashmap::mapref::entry::Entry;
            match self.entries.entry(id) {
                Entry::Vacant(v) => {
                    v.insert(entry);
                }
                Entry::Occupied(_) => { /* skip duplicates */ }
            }
        }
        if count > 0 {
            tracing::info!("Restored {count} mount config(s) from {}", path.display());
        }
    }

    fn persist(&self) {
        if let Some(ref path) = self.persist_path {
            persistence::save(&self.entries, path);
        }
    }

    pub fn config_entries(&self) -> &DashMap<String, MountEntry> {
        &self.entries
    }

    // ── CRUD ──

    pub fn add(&self, entry: MountEntry) -> Result<String, String> {
        let id = entry.id.clone();
        use dashmap::mapref::entry::Entry;
        match self.entries.entry(id.clone()) {
            Entry::Vacant(v) => {
                v.insert(entry);
                self.persist();
                Ok(id)
            }
            Entry::Occupied(_) => Err(format!("Mount {id} already exists")),
        }
    }

    pub fn get(&self, id: &str) -> Option<MountInfo> {
        let entry = self.entries.get(id)?;
        let state = self
            .handles
            .get(id)
            .map(|h| h.state())
            .unwrap_or(MountState::Stopped);
        Some(MountInfo {
            id: entry.id.clone(),
            url: entry.url.clone(),
            protocol: crate::backend::detect_protocol(&entry.url),
            mountpoint: entry.mountpoint.display().to_string(),
            state,
            read_only: entry.read_only,
            cache_ttl_secs: entry.cache_ttl_secs,
            cache_size: entry.cache_size,
        })
    }

    pub fn list(&self) -> Vec<MountInfo> {
        self.entries
            .iter()
            .filter_map(|r| self.get(r.key()))
            .collect()
    }

    pub fn update(&self, id: &str, update: MountEntry) -> Result<(), String> {
        let state = self
            .handles
            .get(id)
            .map(|h| h.state())
            .unwrap_or(MountState::Stopped);
        if !matches!(state, MountState::Stopped | MountState::Error(_)) {
            return Err(format!("Cannot update mount {}: state is {:?}", id, state));
        }
        if id != update.id {
            return Err("Cannot change mount ID".to_string());
        }
        self.entries.insert(id.to_string(), update);
        self.persist();
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<MountEntry, String> {
        let state = self
            .handles
            .get(id)
            .map(|h| h.state())
            .unwrap_or(MountState::Stopped);
        if !matches!(state, MountState::Stopped | MountState::Error(_)) {
            return Err(format!("Cannot remove mount {}: state is {:?}", id, state));
        }
        self.handles.remove(id);
        let result = self
            .entries
            .remove(id)
            .map(|(_, e)| e)
            .ok_or_else(|| format!("Mount {} not found", id));
        if result.is_ok() {
            self.persist();
        }
        result
    }

    // ── Lifecycle ──

    pub fn start(&self, id: &str, backend: Box<dyn StorageBackend>) -> Result<(), String> {
        let backend = backend;
        self.start_deferred(id, move || Ok(backend))
    }

    /// Like `start`, but defers backend creation into the mount thread.
    /// The `make_backend` closure is called inside the mount thread so that
    /// any reqwest::Client (or other runtime-bound resources) are created on
    /// the mount thread's own tokio runtime — avoiding the Windows Instant
    /// overflow caused by moving a Client across runtimes.
    pub fn start_deferred(
        &self,
        id: &str,
        make_backend: impl FnOnce() -> Result<Box<dyn StorageBackend>, String> + Send + 'static,
    ) -> Result<(), String> {
        use dashmap::mapref::entry::Entry;

        let id_owned = id.to_string();

        // Clean up stale handle from a previous stop/error so the Vacant path runs
        if let Some(h) = self.handles.get(&id_owned) {
            let state = h.state();
            if matches!(state, MountState::Stopped | MountState::Error(_)) {
                drop(h);
                self.handles.remove(&id_owned);
            }
        }

        match self.handles.entry(id_owned.clone()) {
            Entry::Vacant(v) => {
                let entry = self
                    .entries
                    .get(id)
                    .ok_or_else(|| format!("Mount {id} not found"))?
                    .clone();

                let mount_state = Arc::new(std::sync::Mutex::new(MountState::Starting));
                let cancel = Arc::new(AtomicBool::new(false));
                let unmount_slot: Arc<std::sync::Mutex<Option<UnmountCallback>>> =
                    Arc::new(std::sync::Mutex::new(None));

                let ms_ready = mount_state.clone();
                let unmount_setter = unmount_slot.clone();
                let config = MountConfig {
                    mountpoint: entry.mountpoint.clone(),
                    read_only: entry.read_only,
                    cache_ttl: std::time::Duration::from_secs(entry.cache_ttl_secs),
                    cache_size: entry.cache_size,
                    allow_other: false,
                    mount_uid: unsafe { libc::getuid() },
                    mount_gid: unsafe { libc::getgid() },
                    on_mount_ready: Some(Arc::new(move || {
                        *ms_ready.lock().unwrap() = MountState::Running;
                    })),
                    on_set_unmount: Some(Arc::new(move |cb: UnmountCallback| {
                        *unmount_setter.lock().unwrap() = Some(cb);
                    })),
                };

                let ms = mount_state.clone();
                let thread_id = id_owned.clone();
                let thread_handle = std::thread::Builder::new()
                    .name(format!("mount-{thread_id}"))
                    .spawn(move || {
                        let backend = match make_backend() {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::error!("Backend creation failed for {thread_id}: {e}");
                                *ms.lock().unwrap() =
                                    MountState::Error(format!("Backend creation: {e}"));
                                return;
                            }
                        };
                        let engine = MountEngine::new(backend, config);

                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            engine.mount()
                        }));
                        match result {
                            Ok(Ok(())) => {
                                *ms.lock().unwrap() = MountState::Stopped;
                            }
                            Ok(Err(e)) => {
                                *ms.lock().unwrap() = MountState::Error(e.to_string());
                            }
                            Err(panic_payload) => {
                                let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                                    s.to_string()
                                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                                    s.clone()
                                } else {
                                    "unknown panic".to_string()
                                };
                                tracing::error!("Mount thread panicked: {msg}");
                                *ms.lock().unwrap() = MountState::Error(format!("panic: {msg}"));
                            }
                        }
                    })
                    .map_err(|e| format!("Failed to spawn mount thread: {e}"))?;

                v.insert(MountHandle {
                    state: mount_state,
                    cancel,
                    thread: Some(thread_handle),
                    unmount_slot,
                });
                Ok(())
            }
            Entry::Occupied(o) => {
                let state = o.get().state();
                if matches!(state, MountState::Stopped | MountState::Error(_)) {
                    drop(o);
                    self.handles.remove(&id_owned);
                    // State is clean — caller can retry immediately
                    Err(format!(
                        "Mount {id} is {state:?}, removed old handle. Retry the request."
                    ))
                } else {
                    Err(format!("Cannot start mount {id}: state is {state:?}"))
                }
            }
        }
    }

    pub fn stop(&self, id: &str) -> Result<(), String> {
        // Phase 1: Validate state, set Stopping, extract thread handle + unmount slot ref
        let (thread_handle, unmount_slot) = {
            let mut handle = self
                .handles
                .get_mut(id)
                .ok_or_else(|| format!("Mount {} not found or not started", id))?;

            let state = handle.state();
            if state != MountState::Running && state != MountState::Starting {
                return Err(format!("Cannot stop mount {}: state is {:?}", id, state));
            }

            *handle.state.lock().unwrap() = MountState::Stopping;
            handle.cancel.store(true, Ordering::Release);
            (handle.thread.take(), Arc::clone(&handle.unmount_slot))
        }; // DashMap lock released

        // Phase 2: Unmount (outside lock)
        let mountpoint = self
            .entries
            .get(id)
            .map(|e| e.mountpoint.display().to_string())
            .unwrap_or_default();

        #[cfg(target_os = "linux")]
        {
            let _ = &unmount_slot;
            if !mountpoint.is_empty() {
                let _ = std::process::Command::new("fusermount")
                    .args(["-u", &mountpoint])
                    .output();
            }
        }
        #[cfg(target_os = "windows")]
        {
            let unmount_fn = unmount_slot.lock().unwrap().clone();
            if let Some(ref unmount) = unmount_fn {
                tracing::debug!("Calling WinFsp unmount for {id}");
                unmount();
            } else {
                tracing::warn!(
                    "Unmount callback not yet registered for {id} — mount may still be starting"
                );
            }
        }

        // Phase 3: Join with timeout (spawn reaper to avoid blocking forever)
        if let Some(th) = thread_handle {
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            std::thread::spawn(move || {
                let _ = th.join();
                let _ = tx.send(());
            });
            if rx.recv_timeout(std::time::Duration::from_secs(10)).is_err() {
                tracing::warn!("Mount thread for {id} did not exit in 10s, detaching");
                if let Some(h) = self.handles.get_mut(id) {
                    *h.state.lock().unwrap() =
                        MountState::Error("Stop timed out — mount may still be active".into());
                }
                return Ok(());
            }
        }

        if let Some(h) = self.handles.get_mut(id) {
            *h.state.lock().unwrap() = MountState::Stopped;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(id: &str) -> MountEntry {
        MountEntry {
            id: id.to_string(),
            url: "http://localhost:9000".to_string(),
            mountpoint: PathBuf::from(format!("/mnt/{id}")),
            read_only: false,
            username: None,
            password: None,
            cache_ttl_secs: 5,
            cache_size: 256,
        }
    }

    #[test]
    fn test_add_mount() {
        let mgr = MountManager::new();
        let id = mgr.add(test_entry("m1")).unwrap();
        let info = mgr.get(&id).unwrap();
        assert_eq!(info.url, "http://localhost:9000");
        assert_eq!(info.state, MountState::Stopped);
        assert!(!info.read_only);
    }

    #[test]
    fn test_add_duplicate_rejected() {
        let mgr = MountManager::new();
        mgr.add(test_entry("m1")).unwrap();
        let err = mgr.add(test_entry("m1")).unwrap_err();
        assert!(err.contains("already exists"));
    }

    #[test]
    fn test_list_mounts() {
        let mgr = MountManager::new();
        mgr.add(test_entry("a")).unwrap();
        mgr.add(test_entry("b")).unwrap();
        let list = mgr.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_update_mount() {
        let mgr = MountManager::new();
        mgr.add(test_entry("m1")).unwrap();

        let mut updated = test_entry("m1");
        updated.read_only = true;
        mgr.update("m1", updated).unwrap();

        let info = mgr.get("m1").unwrap();
        assert!(info.read_only);
    }

    #[test]
    fn test_update_running_rejected() {
        let mgr = MountManager::new();
        mgr.add(test_entry("m1")).unwrap();
        // Simulate running state
        mgr.handles.insert(
            "m1".to_string(),
            MountHandle {
                state: Arc::new(std::sync::Mutex::new(MountState::Running)),
                cancel: Arc::new(AtomicBool::new(false)),
                thread: None,
                unmount_slot: Arc::new(std::sync::Mutex::new(None)),
            },
        );
        let err = mgr.update("m1", test_entry("m1")).unwrap_err();
        assert!(err.contains("Cannot update"));
    }

    #[test]
    fn test_remove_mount() {
        let mgr = MountManager::new();
        mgr.add(test_entry("m1")).unwrap();
        let removed = mgr.remove("m1").unwrap();
        assert_eq!(removed.id, "m1");
        assert!(mgr.get("m1").is_none());
    }

    #[test]
    fn test_remove_not_found() {
        let mgr = MountManager::new();
        let err = mgr.remove("nonexistent").unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_get_not_found() {
        let mgr = MountManager::new();
        assert!(mgr.get("nonexistent").is_none());
    }

    #[test]
    fn test_stop_not_started() {
        let mgr = MountManager::new();
        mgr.add(test_entry("m1")).unwrap();
        let err = mgr.stop("m1").unwrap_err();
        assert!(err.contains("not found or not started"));
    }

    #[test]
    fn test_start_already_running() {
        let mgr = MountManager::new();
        mgr.add(test_entry("m1")).unwrap();
        // Simulate running state
        mgr.handles.insert(
            "m1".to_string(),
            MountHandle {
                state: Arc::new(std::sync::Mutex::new(MountState::Running)),
                cancel: Arc::new(AtomicBool::new(false)),
                thread: None,
                unmount_slot: Arc::new(std::sync::Mutex::new(None)),
            },
        );
        // start() rejects because state is Running (never actually creates backend)
        let err = mgr
            .start("m1", Box::new(crate::mount::MockBackend::new()))
            .unwrap_err();
        assert!(err.contains("Cannot start"));
    }

    #[test]
    fn test_start_not_found() {
        let mgr = MountManager::new();
        let err = mgr
            .start("nonexistent", Box::new(crate::mount::MockBackend::new()))
            .unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_detect_protocol() {
        assert_eq!(crate::backend::detect_protocol("http://host"), "webdav");
        assert_eq!(crate::backend::detect_protocol("https://host"), "webdav");
        assert_eq!(crate::backend::detect_protocol("webdav://host"), "webdav");
        assert_eq!(crate::backend::detect_protocol("s3://bucket"), "s3");
        assert_eq!(crate::backend::detect_protocol("ftp://host"), "ftp");
        assert_eq!(crate::backend::detect_protocol("host:5000"), "unknown");
    }
}
