use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub type UnmountCallback = Arc<dyn Fn() + Send + Sync>;
pub type SetUnmountCallback = Arc<dyn Fn(UnmountCallback) + Send + Sync>;

fn recover_lock<T>(
    r: std::sync::LockResult<std::sync::MutexGuard<'_, T>>,
) -> std::sync::MutexGuard<'_, T> {
    r.unwrap_or_else(|e| {
        tracing::warn!("Recovering from poisoned internal lock");
        e.into_inner()
    })
}

use serde::Serialize;
use tokio::sync::broadcast;

use crate::backend::{Entry, StorageBackend};
use crate::cache::{CacheLayer, CachedAttr, CachedChildren};
use crate::error::{BackendError, MountError};
use crate::handle::{HandleTable, WriteAtError};
use crate::inode::InodeMap;
use crate::prefetch::BandwidthEstimator;

// ---------------------------------------------------------------------------
// MountEvent
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub enum MountEvent {
    Connected {
        url: String,
    },
    MountStarted {
        mountpoint: PathBuf,
    },
    MountStopped,
    FileRead {
        path: PathBuf,
        bytes: u64,
        duration_ms: u64,
    },
    FileWritten {
        path: PathBuf,
        bytes: u64,
        duration_ms: u64,
    },
    DirListed {
        path: PathBuf,
        entries: usize,
    },
    CacheHit {
        path: PathBuf,
    },
    CacheMiss {
        path: PathBuf,
    },
    Error {
        error: String,
    },
}

// ---------------------------------------------------------------------------
// FuseAdapter — platform-agnostic core
// ---------------------------------------------------------------------------

/// A pending background prefetch task.
/// Uses Arc<Mutex> shared state instead of block_on() to avoid
/// nested runtime panic when called from within an existing block_on() context
/// (e.g., WinFsp callback → block_on(read()) → try_collect_prefetch()).
#[allow(clippy::type_complexity)]
struct PrefetchSlot {
    result: Arc<std::sync::Mutex<Option<(u64, Vec<u8>)>>>,
    _handle: tokio::task::JoinHandle<()>,
}

pub struct FuseAdapter<B: StorageBackend> {
    pub(crate) backend: Arc<B>,
    pub(crate) cache: CacheLayer,
    pub(crate) handles: HandleTable,
    pub(crate) inodes: Arc<InodeMap>,
    pub(crate) read_only: bool,
    event_tx: broadcast::Sender<MountEvent>,
    pub(crate) rt: tokio::runtime::Runtime,
    bandwidth: std::sync::Mutex<BandwidthEstimator>,
    prefetch: std::sync::Mutex<HashMap<u64, PrefetchSlot>>,
}

impl<B: StorageBackend> FuseAdapter<B> {
    pub fn new(backend: B, config: &MountConfig) -> Self {
        let (event_tx, _) = broadcast::channel(64);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("failed to create tokio runtime");
        Self {
            backend: Arc::new(backend),
            cache: CacheLayer::new(config.cache_ttl, config.cache_size as u64),
            handles: HandleTable::new(),
            inodes: Arc::new(InodeMap::new(PathBuf::from("/"))),
            read_only: config.read_only,
            event_tx,
            rt,
            bandwidth: std::sync::Mutex::new(BandwidthEstimator::new()),
            prefetch: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MountEvent> {
        self.event_tx.subscribe()
    }

    pub fn emit(&self, event: MountEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Block on an async future using our internal runtime.
    pub(crate) fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        self.rt.block_on(f)
    }

    /// Non-blocking check for completed prefetch. Returns Some(data) if the
    /// prefetch finished and covers the requested range. If not ready yet,
    /// the slot is kept for a future check.
    fn try_collect_prefetch(&self, fh: u64, offset: u64, size: u32) -> Option<Vec<u8>> {
        let slot = {
            let mut slots = recover_lock(self.prefetch.lock());
            slots.remove(&fh)?
        };

        // Non-blocking: check if the prefetch task wrote its result
        // Use mem::take to take ownership instead of cloning up to 16 MB.
        let mut guard = recover_lock(slot.result.lock());
        match std::mem::take(&mut *guard) {
            Some((prefetch_offset, data)) if !data.is_empty() => {
                drop(guard); // release lock before writing to read_cache
                tracing::debug!(
                    "[prefetch] collected {} bytes at offset {prefetch_offset} for fh={fh}",
                    data.len(),
                );
                self.handles.set_read_cache(fh, data, prefetch_offset);
                self.handles.read_from_cache(fh, offset, size)
            }
            Some(_) => None, // completed but empty — discard slot
            None => {
                // Not ready yet — put slot back
                drop(guard);
                recover_lock(self.prefetch.lock()).insert(fh, slot);
                None
            }
        }
    }

    /// If sequential access is detected and current cache is past 50%, spawn
    /// a background task to fetch the next chunk.
    fn maybe_spawn_prefetch(&self, fh: u64, path: &str) {
        let cache_info = match self.handles.get_cache_info(fh) {
            Some(info) => info,
            None => return,
        };
        let (cache_start, cache_len) = cache_info;
        if cache_len == 0 {
            return;
        }

        // Check consumption threshold (past 50% of cache)
        let (last_read_end, is_seq) = match self.handles.get_read_state(fh) {
            Some(state) => state,
            None => return,
        };
        if !is_seq {
            return;
        }

        let consumed = last_read_end.saturating_sub(cache_start);
        if consumed < (cache_len as u64 * 5) / 10 {
            return;
        }

        // Don't double-prefetch
        {
            let slots = recover_lock(self.prefetch.lock());
            if slots.contains_key(&fh) {
                return;
            }
        }

        let next_offset = cache_start + cache_len as u64;
        let bw = recover_lock(self.bandwidth.lock());
        let prefetch_size = bw.prefetch_size(5.0, u64::MAX);
        drop(bw);

        let backend = self.backend.clone();
        let path_owned = path.to_string();

        tracing::debug!("[prefetch] spawning: fh={fh} offset={next_offset} size={prefetch_size}");

        let result = Arc::new(std::sync::Mutex::new(None));
        let result_clone = result.clone();

        let _handle = self.rt.spawn(async move {
            let read_future = backend.read(&path_owned, next_offset, prefetch_size);
            let prefetch_result =
                match tokio::time::timeout(std::time::Duration::from_secs(60), read_future).await {
                    Ok(Ok(data)) if !data.is_empty() => Some((next_offset, data)),
                    Ok(Ok(_)) => {
                        tracing::debug!(
                            "[prefetch] empty response for {path_owned} at {next_offset}"
                        );
                        None
                    }
                    Ok(Err(e)) => {
                        tracing::debug!("[prefetch] failed for {path_owned} at {next_offset}: {e}");
                        None
                    }
                    Err(_) => {
                        tracing::debug!("[prefetch] timed out for {path_owned} at {next_offset}");
                        None
                    }
                };
            *recover_lock(result_clone.lock()) = prefetch_result;
        });

        recover_lock(self.prefetch.lock()).insert(fh, PrefetchSlot { result, _handle });
    }

    /// Abort any pending prefetch for a file handle.
    fn abort_prefetch(&self, fh: u64) {
        if let Some(slot) = recover_lock(self.prefetch.lock()).remove(&fh) {
            slot._handle.abort();
        }
    }

    /// Abort all pending prefetch tasks. Called during shutdown.
    pub fn abort_all_prefetch(&self) {
        let mut slots = recover_lock(self.prefetch.lock());
        let count = slots.len();
        for (_, slot) in slots.drain() {
            slot._handle.abort();
        }
        if count > 0 {
            tracing::info!("Aborted {count} pending prefetch task(s)");
        }
    }

    pub fn discard_handle(&self, fh: u64) {
        self.abort_prefetch(fh);
        let _ = self.handles.remove(fh);
    }

    // --- Inherent async methods (for testing + Windows adapter) ---

    pub async fn getattr(&self, path: &str) -> Result<Entry, MountError> {
        if let Some(cached) = self.cache.get_attr(path).await {
            self.emit(MountEvent::CacheHit { path: path.into() });
            return Ok(cached.entry);
        }
        self.emit(MountEvent::CacheMiss { path: path.into() });
        let entry = self.backend.stat(path).await?;
        self.cache
            .set_attr(
                path,
                CachedAttr {
                    entry: entry.clone(),
                },
            )
            .await;
        Ok(entry)
    }

    pub async fn readdir(&self, path: &str) -> Result<Vec<Entry>, MountError> {
        if let Some(cached) = self.cache.get_children(path).await {
            return Ok(cached.entries);
        }
        let entries = self.backend.list(path).await?;
        let count = entries.len();

        // Prime attr cache for each child — eliminates N+1 getattr calls
        // when the shell queries attributes for every visible file.
        let parent = path.trim_end_matches('/');
        for entry in &entries {
            let child_path = if parent.is_empty() || parent == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", parent, entry.name)
            };
            self.cache
                .set_attr(
                    &child_path,
                    CachedAttr {
                        entry: entry.clone(),
                    },
                )
                .await;
        }

        self.cache
            .set_children(
                path,
                CachedChildren {
                    entries: entries.clone(),
                },
            )
            .await;
        self.emit(MountEvent::DirListed {
            path: path.into(),
            entries: count,
        });
        Ok(entries)
    }

    pub async fn open(&self, path: &str, write: bool) -> Result<u64, MountError> {
        if write && self.read_only {
            return Err(MountError::Backend(BackendError::ReadOnly));
        }
        Ok(self.handles.allocate(path.to_string()))
    }

    pub async fn read(&self, fh: u64, offset: u64, size: u32) -> Result<Vec<u8>, MountError> {
        let path = self.handles.get_path(fh).ok_or_else(|| {
            MountError::Backend(BackendError::NotFound("Invalid file handle".into()))
        })?;

        // 0. Check dirty write buffer first (unflushed writes must be visible)
        if let Some(data) = self.handles.read_from_dirty(fh, offset, size) {
            return Ok(data);
        }

        let is_sequential = self.handles.update_read_pattern(fh, offset, size);
        let is_first = self.handles.is_first_read(fh);

        // 1. Try per-handle read cache
        if let Some(cached) = self.handles.read_from_cache(fh, offset, size) {
            self.maybe_spawn_prefetch(fh, &path);
            return Ok(cached);
        }

        // 2. Check if a background prefetch completed
        if let Some(result) = self.try_collect_prefetch(fh, offset, size) {
            return Ok(result);
        }

        // 3. Cache miss — adaptive fetch
        let fetch_size = if is_first && size <= 256 * 1024 {
            // Quick first response: small initial reads (e.g. PotPlayer header probe)
            // don't prefetch — respond immediately to avoid timeout errors.
            size
        } else {
            let pipeline_secs = if is_sequential { 5.0 } else { 2.0 };
            let file_size = self
                .cache
                .get_attr(&path)
                .await
                .map(|c| c.entry.size)
                .unwrap_or(0);
            let remaining = if file_size > offset {
                file_size - offset
            } else {
                u64::MAX // Unknown size: don't constrain prefetch
            };
            recover_lock(self.bandwidth.lock())
                .prefetch_size(pipeline_secs, remaining)
                .max(size)
        };

        let start = std::time::Instant::now();
        let data = self.backend.read(&path, offset, fetch_size).await?;
        let elapsed = start.elapsed();

        // Record bandwidth observation
        if !data.is_empty() {
            recover_lock(self.bandwidth.lock()).observe(data.len() as u64, elapsed);
        }

        // Split data: full prefetch goes to cache, caller gets only requested slice.
        // Avoids cloning the full prefetch when caller needs less (common for sequential reads).
        let result = if data.len() > size as usize {
            let result = data[..size as usize].to_vec();
            self.handles.set_read_cache(fh, data, offset);
            result
        } else {
            let result = data;
            self.handles.set_read_cache(fh, result.clone(), offset);
            result
        };

        // If sequential, start background prefetch for the next chunk
        if is_sequential {
            self.maybe_spawn_prefetch(fh, &path);
        }

        self.emit(MountEvent::FileRead {
            path: (&*path).into(),
            bytes: result.len() as u64,
            duration_ms: elapsed.as_millis() as u64,
        });
        Ok(result)
    }

    pub async fn write(&self, fh: u64, offset: u64, data: &[u8]) -> Result<(), MountError> {
        match self.handles.write_at(fh, offset, data) {
            Ok(()) => {}
            Err(WriteAtError::InvalidHandle) => {
                return Err(MountError::Backend(BackendError::NotFound(
                    "Invalid file handle".into(),
                )));
            }
            Err(WriteAtError::TooLarge) => {
                return Err(MountError::Backend(BackendError::NotSupported(
                    "Write exceeds 2 GiB in-memory buffer limit".into(),
                )));
            }
        }
        Ok(())
    }

    pub async fn flush(&self, fh: u64) -> Result<(), MountError> {
        if self.handles.get_path(fh).is_none() {
            return Err(MountError::Backend(BackendError::NotFound(
                "Invalid file handle".into(),
            )));
        }
        if let Some((path, buffer)) = self.handles.take_dirty(fh) {
            let bytes = buffer.len() as u64;
            if let Err(e) = self.backend.write(&path, &buffer).await {
                self.handles.restore_dirty(fh, buffer);
                return Err(MountError::Backend(e));
            }
            self.cache.invalidate(&path).await;
            self.cache.invalidate_parent(&path).await;
            self.emit(MountEvent::FileWritten {
                path: (&*path).into(),
                bytes,
                duration_ms: 0,
            });
        }
        Ok(())
    }

    pub async fn release(&self, fh: u64) -> Result<(), MountError> {
        self.abort_prefetch(fh);
        let open_file = self.handles.remove(fh);
        if let Some(file) = open_file {
            if file.dirty {
                if let Err(e) = self.backend.write(&file.path, &file.buffer).await {
                    tracing::error!(
                        "release: write failed for {}, {} bytes lost: {e}",
                        file.path,
                        file.buffer.len()
                    );
                    return Err(MountError::Backend(e));
                }
                self.cache.invalidate(&file.path).await;
                self.cache.invalidate_parent(&file.path).await;
            }
        }
        Ok(())
    }

    pub async fn mkdir(&self, path: &str) -> Result<(), MountError> {
        if self.read_only {
            return Err(MountError::Backend(BackendError::ReadOnly));
        }
        self.backend.mkdir(path).await?;
        self.cache.invalidate_parent(path).await;
        Ok(())
    }

    pub async fn rmdir(&self, path: &str) -> Result<(), MountError> {
        if self.read_only {
            return Err(MountError::Backend(BackendError::ReadOnly));
        }
        self.backend.delete(path).await?;
        self.cache.invalidate(path).await;
        self.cache.invalidate_parent(path).await;
        Ok(())
    }

    pub async fn unlink(&self, path: &str) -> Result<(), MountError> {
        if self.read_only {
            return Err(MountError::Backend(BackendError::ReadOnly));
        }
        self.backend.delete(path).await?;
        self.cache.invalidate(path).await;
        self.cache.invalidate_parent(path).await;
        Ok(())
    }

    pub async fn rename_entry(&self, from: &str, to: &str) -> Result<(), MountError> {
        if self.read_only {
            return Err(MountError::Backend(BackendError::ReadOnly));
        }
        self.backend.rename(from, to).await?;
        self.cache.invalidate(from).await;
        self.cache.invalidate(to).await;
        self.cache.invalidate_parent(from).await;
        self.cache.invalidate_parent(to).await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MountConfig
// ---------------------------------------------------------------------------

pub struct MountConfig {
    pub mountpoint: PathBuf,
    pub read_only: bool,
    pub cache_ttl: Duration,
    pub cache_size: usize,
    pub allow_other: bool,
    pub on_mount_ready: Option<UnmountCallback>,
    pub on_set_unmount: Option<SetUnmountCallback>,
}

// ---------------------------------------------------------------------------
// MountStatus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum MountStatus {
    Idle,
    Mounting,
    Mounted,
    Unmounting,
    Error(String),
}

// ---------------------------------------------------------------------------
// MountEngine — platform dispatching
// ---------------------------------------------------------------------------

pub struct MountEngine<B: StorageBackend> {
    pub(crate) backend: Option<B>,
    pub(crate) config: MountConfig,
    pub(crate) status: std::sync::Mutex<MountStatus>,
    pub(crate) event_tx: broadcast::Sender<MountEvent>,
}

impl<B: StorageBackend> MountEngine<B> {
    pub fn new(backend: B, config: MountConfig) -> Self {
        let (event_tx, _) = broadcast::channel(64);
        Self {
            backend: Some(backend),
            config,
            status: std::sync::Mutex::new(MountStatus::Idle),
            event_tx,
        }
    }

    pub fn status(&self) -> MountStatus {
        recover_lock(self.status.lock()).clone()
    }
    pub fn mountpoint(&self) -> &Path {
        &self.config.mountpoint
    }
    pub fn subscribe(&self) -> broadcast::Receiver<MountEvent> {
        self.event_tx.subscribe()
    }

    pub fn mount(self) -> Result<(), MountError> {
        let Self {
            backend,
            config,
            status,
            event_tx,
        } = self;
        let backend = backend.expect("mount() called on consumed engine");
        *recover_lock(status.lock()) = MountStatus::Mounting;
        event_tx
            .send(MountEvent::MountStarted {
                mountpoint: config.mountpoint.clone(),
            })
            .ok();

        let adapter = FuseAdapter::new(backend, &config);

        #[cfg(target_os = "linux")]
        return crate::mount_linux::mount_linux(adapter, &config, &status, &event_tx);
        #[cfg(target_os = "windows")]
        return crate::mount_windows::mount_windows(adapter, &config, &status, &event_tx);
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            let _ = adapter;
            Err(MountError::Config(
                "Unsupported platform: only Linux and Windows are supported".to_string(),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Mock Backend (shared across test modules)
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) struct MockBackend {
    pub entries: std::sync::Mutex<Vec<Entry>>,
    content: std::sync::Mutex<Vec<(String, Vec<u8>)>>,
    write_fails: std::sync::Mutex<bool>,
}

#[cfg(test)]
impl MockBackend {
    pub fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(Vec::new()),
            content: std::sync::Mutex::new(Vec::new()),
            write_fails: std::sync::Mutex::new(false),
        }
    }

    pub fn set_write_fails(&self, v: bool) {
        *recover_lock(self.write_fails.lock()) = v;
    }

    pub fn add_file(&self, path: &str, name: &str, size: u64, data: &[u8]) {
        recover_lock(self.entries.lock()).push(Entry {
            path: path.to_string(),
            name: name.to_string(),
            dir: false,
            size,
            mtime: std::time::SystemTime::UNIX_EPOCH,
        });
        recover_lock(self.content.lock()).push((path.to_string(), data.to_vec()));
    }

    pub fn add_dir(&self, path: &str, name: &str) {
        recover_lock(self.entries.lock()).push(Entry {
            path: path.to_string(),
            name: name.to_string(),
            dir: true,
            size: 0,
            mtime: std::time::SystemTime::UNIX_EPOCH,
        });
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl StorageBackend for MockBackend {
    fn protocol(&self) -> &str {
        "mock"
    }
    fn server_addr(&self) -> &str {
        "mock://test"
    }

    async fn list(&self, path: &str) -> Result<Vec<Entry>, BackendError> {
        let entries = recover_lock(self.entries.lock());
        let parent = path.trim_end_matches('/');
        let children: Vec<Entry> = entries
            .iter()
            .filter(|e| {
                let p = std::path::Path::new(&e.path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default()
                    .trim_end_matches('/')
                    .to_string();
                let parent_normalized = if parent.is_empty() { "" } else { parent };
                p == parent_normalized
                    || (p.is_empty() && parent_normalized == "/")
                    || (p == "/" && parent_normalized.is_empty())
            })
            .cloned()
            .collect();
        Ok(children)
    }

    async fn stat(&self, path: &str) -> Result<Entry, BackendError> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|e| e.path == path)
            .cloned()
            .ok_or_else(|| BackendError::NotFound(path.to_string()))
    }

    async fn read(&self, path: &str, offset: u64, size: u32) -> Result<Vec<u8>, BackendError> {
        let content = recover_lock(self.content.lock());
        let data = content
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, d)| d.clone())
            .ok_or_else(|| BackendError::NotFound(path.to_string()))?;
        let start = offset as usize;
        let end = std::cmp::min(start + size as usize, data.len());
        if start >= data.len() {
            return Ok(Vec::new());
        }
        Ok(data[start..end].to_vec())
    }

    async fn write(&self, path: &str, data: &[u8]) -> Result<(), BackendError> {
        if *recover_lock(self.write_fails.lock()) {
            return Err(BackendError::ConnectionFailed(
                "mock write failure".to_string(),
            ));
        }
        let mut content = recover_lock(self.content.lock());
        if let Some(entry) = content.iter_mut().find(|(p, _)| p == path) {
            entry.1 = data.to_vec();
        } else {
            content.push((path.to_string(), data.to_vec()));
        }
        let mut entries = recover_lock(self.entries.lock());
        if let Some(entry) = entries.iter_mut().find(|e| e.path == path) {
            entry.size = data.len() as u64;
        } else {
            let name = path.rsplit('/').next().unwrap_or(path).to_string();
            entries.push(Entry {
                path: path.to_string(),
                name,
                dir: false,
                size: data.len() as u64,
                mtime: std::time::SystemTime::UNIX_EPOCH,
            });
        }
        Ok(())
    }

    async fn mkdir(&self, path: &str) -> Result<(), BackendError> {
        let name = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .to_string();
        recover_lock(self.entries.lock()).push(Entry {
            path: path.to_string(),
            name,
            dir: true,
            size: 0,
            mtime: std::time::SystemTime::UNIX_EPOCH,
        });
        Ok(())
    }

    async fn delete(&self, path: &str) -> Result<(), BackendError> {
        recover_lock(self.entries.lock()).retain(|e| e.path != path);
        recover_lock(self.content.lock()).retain(|(p, _)| p != path);
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), BackendError> {
        let mut entries = recover_lock(self.entries.lock());
        if let Some(entry) = entries.iter_mut().find(|e| e.path == from) {
            entry.path = to.to_string();
            entry.name = to.rsplit('/').next().unwrap_or(to).to_string();
        }
        let mut content = recover_lock(self.content.lock());
        if let Some(item) = content.iter_mut().find(|(p, _)| p == from) {
            item.0 = to.to_string();
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

    fn make_config() -> MountConfig {
        MountConfig {
            mountpoint: PathBuf::from("/mnt/test"),
            read_only: false,
            cache_ttl: Duration::from_secs(60),
            cache_size: 100,
            allow_other: false,
            on_mount_ready: None,
            on_set_unmount: None,
        }
    }

    fn make_ro_config() -> MountConfig {
        MountConfig {
            mountpoint: PathBuf::from("/mnt/test"),
            read_only: true,
            cache_ttl: Duration::from_secs(60),
            cache_size: 100,
            allow_other: false,
            on_mount_ready: None,
            on_set_unmount: None,
        }
    }

    // ── FuseAdapter inherent method tests ──
    // Use #[test] (not #[tokio::test]) because FuseAdapter owns its own Runtime.
    // Dropping it inside a tokio context would panic.

    #[test]
    fn test_getattr_cache_hit() {
        let backend = MockBackend::new();
        backend.add_file("/f.txt", "f.txt", 5, b"hello");
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        let e1 = rt.block_on(adapter.getattr("/f.txt")).unwrap();
        let e2 = rt.block_on(adapter.getattr("/f.txt")).unwrap();
        assert_eq!(e1.size, e2.size);
    }

    #[test]
    fn test_getattr_not_found() {
        let backend = MockBackend::new();
        let adapter = FuseAdapter::new(backend, &make_config());
        let result = adapter.rt.block_on(adapter.getattr("/nope"));
        assert!(result.is_err());
    }

    #[test]
    fn test_readdir_children() {
        let backend = MockBackend::new();
        backend.add_file("/a.txt", "a.txt", 1, b"a");
        backend.add_file("/b.txt", "b.txt", 2, b"bb");
        let adapter = FuseAdapter::new(backend, &make_config());

        let children = adapter.rt.block_on(adapter.readdir("/")).unwrap();
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn test_open_read_release() {
        let backend = MockBackend::new();
        backend.add_file("/f.txt", "f.txt", 5, b"hello");
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        let fh = rt.block_on(adapter.open("/f.txt", false)).unwrap();
        let data = rt.block_on(adapter.read(fh, 0, 5)).unwrap();
        assert_eq!(data, b"hello");
        rt.block_on(adapter.release(fh)).unwrap();
    }

    #[test]
    fn test_read_offset() {
        let backend = MockBackend::new();
        backend.add_file("/f.txt", "f.txt", 11, b"hello world");
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        let fh = rt.block_on(adapter.open("/f.txt", false)).unwrap();
        let data = rt.block_on(adapter.read(fh, 6, 5)).unwrap();
        assert_eq!(data, b"world");
        rt.block_on(adapter.release(fh)).unwrap();
    }

    #[test]
    fn test_read_beyond_eof() {
        let backend = MockBackend::new();
        backend.add_file("/f.txt", "f.txt", 5, b"hello");
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        let fh = rt.block_on(adapter.open("/f.txt", false)).unwrap();
        let data = rt.block_on(adapter.read(fh, 100, 10)).unwrap();
        assert!(data.is_empty());
        rt.block_on(adapter.release(fh)).unwrap();
    }

    #[test]
    fn test_read_invalid_fh() {
        let backend = MockBackend::new();
        let adapter = FuseAdapter::new(backend, &make_config());
        let result = adapter.rt.block_on(adapter.read(9999, 0, 10));
        assert!(result.is_err());
    }

    #[test]
    fn test_write_flush_cycle() {
        let backend = MockBackend::new();
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        let fh = rt.block_on(adapter.open("/new.txt", true)).unwrap();
        rt.block_on(adapter.write(fh, 0, b"data")).unwrap();
        rt.block_on(adapter.flush(fh)).unwrap();
        rt.block_on(adapter.release(fh)).unwrap();

        let entry = rt.block_on(adapter.getattr("/new.txt")).unwrap();
        assert_eq!(entry.size, 4);
    }

    #[test]
    fn test_write_at_offset() {
        let backend = MockBackend::new();
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        let fh = rt.block_on(adapter.open("/new.txt", true)).unwrap();
        rt.block_on(adapter.write(fh, 0, b"AAAA")).unwrap();
        rt.block_on(adapter.write(fh, 4, b"BBBB")).unwrap();
        rt.block_on(adapter.flush(fh)).unwrap();
        rt.block_on(adapter.release(fh)).unwrap();

        let fh2 = rt.block_on(adapter.open("/new.txt", false)).unwrap();
        let data = rt.block_on(adapter.read(fh2, 0, 8)).unwrap();
        assert_eq!(data, b"AAAABBBB");
        rt.block_on(adapter.release(fh2)).unwrap();
    }

    #[test]
    fn test_release_dirty_fallback() {
        let backend = MockBackend::new();
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        let fh = rt.block_on(adapter.open("/lazy.txt", true)).unwrap();
        rt.block_on(adapter.write(fh, 0, b"lazy data")).unwrap();
        rt.block_on(adapter.release(fh)).unwrap();

        let entry = rt.block_on(adapter.getattr("/lazy.txt")).unwrap();
        assert_eq!(entry.size, 9);
    }

    #[test]
    fn test_mkdir_and_readdir() {
        let backend = MockBackend::new();
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        rt.block_on(adapter.mkdir("/newdir")).unwrap();
        let children = rt.block_on(adapter.readdir("/")).unwrap();
        assert!(children.iter().any(|e| e.name == "newdir" && e.dir));
    }

    #[test]
    fn test_rmdir_removes_entry() {
        let backend = MockBackend::new();
        backend.add_dir("/gone", "gone");
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        rt.block_on(adapter.rmdir("/gone")).unwrap();
        assert!(rt.block_on(adapter.getattr("/gone")).is_err());
    }

    #[test]
    fn test_unlink_removes_file() {
        let backend = MockBackend::new();
        backend.add_file("/f.txt", "f.txt", 5, b"hello");
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        rt.block_on(adapter.unlink("/f.txt")).unwrap();
        assert!(rt.block_on(adapter.getattr("/f.txt")).is_err());
    }

    #[test]
    fn test_rename_moves() {
        let backend = MockBackend::new();
        backend.add_file("/old.txt", "old.txt", 5, b"data");
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        rt.block_on(adapter.rename_entry("/old.txt", "/new.txt"))
            .unwrap();
        assert!(rt.block_on(adapter.getattr("/old.txt")).is_err());
        let entry = rt.block_on(adapter.getattr("/new.txt")).unwrap();
        assert_eq!(entry.name, "new.txt");
    }

    #[test]
    fn test_readonly_blocks_write() {
        let backend = MockBackend::new();
        let adapter = FuseAdapter::new(backend, &make_ro_config());
        let result = adapter.rt.block_on(adapter.open("/f.txt", true));
        assert!(result.is_err());
    }

    #[test]
    fn test_readonly_blocks_mkdir() {
        let backend = MockBackend::new();
        let adapter = FuseAdapter::new(backend, &make_ro_config());
        let result = adapter.rt.block_on(adapter.mkdir("/dir"));
        assert!(result.is_err());
    }

    #[test]
    fn test_readonly_blocks_unlink() {
        let backend = MockBackend::new();
        let adapter = FuseAdapter::new(backend, &make_ro_config());
        let result = adapter.rt.block_on(adapter.unlink("/f.txt"));
        assert!(result.is_err());
    }

    #[test]
    fn test_readonly_blocks_rename() {
        let backend = MockBackend::new();
        let adapter = FuseAdapter::new(backend, &make_ro_config());
        let result = adapter.rt.block_on(adapter.rename_entry("/a", "/b"));
        assert!(result.is_err());
    }

    #[test]
    fn test_flush_restore_dirty_on_backend_failure() {
        let backend = MockBackend::new();
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        let fh = rt.block_on(adapter.open("/crash.txt", true)).unwrap();
        rt.block_on(adapter.write(fh, 0, b"important data"))
            .unwrap();

        // Make backend writes fail
        adapter.backend.as_ref().set_write_fails(true);
        let flush_result = rt.block_on(adapter.flush(fh));
        assert!(
            flush_result.is_err(),
            "flush should fail when backend fails"
        );

        // Data should be restored — handle still holds dirty buffer
        adapter.backend.as_ref().set_write_fails(false);
        rt.block_on(adapter.flush(fh)).unwrap();
        rt.block_on(adapter.release(fh)).unwrap();

        // Verify data actually persisted after retry
        let fh2 = rt.block_on(adapter.open("/crash.txt", false)).unwrap();
        let data = rt.block_on(adapter.read(fh2, 0, 14)).unwrap();
        assert_eq!(data, b"important data");
        rt.block_on(adapter.release(fh2)).unwrap();
    }

    #[test]
    fn test_rejected_large_write_does_not_persist_empty_file() {
        let backend = MockBackend::new();
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        let fh = rt.block_on(adapter.open("/too-large.bin", true)).unwrap();
        let result = rt.block_on(adapter.write(fh, 2u64 * 1024 * 1024 * 1024, b"x"));
        assert!(result.is_err(), "oversized write should be rejected");

        rt.block_on(adapter.release(fh)).unwrap();
        assert!(
            rt.block_on(adapter.getattr("/too-large.bin")).is_err(),
            "rejected write must not create a persisted file"
        );
    }

    #[test]
    fn test_rename_invalidates_cache() {
        let backend = MockBackend::new();
        backend.add_file("/dir/old.txt", "old.txt", 4, b"data");
        backend.add_file("/dir/other.txt", "other.txt", 4, b"xxxx");
        let adapter = FuseAdapter::new(backend, &make_config());
        let rt = &adapter.rt;

        // Prime caches: attr for old.txt, children for /dir
        rt.block_on(adapter.getattr("/dir/old.txt")).unwrap();
        rt.block_on(adapter.readdir("/dir")).unwrap();
        assert!(rt
            .block_on(adapter.cache.get_attr("/dir/old.txt"))
            .is_some());
        assert!(rt.block_on(adapter.cache.get_children("/dir")).is_some());

        // Rename — should invalidate both caches
        rt.block_on(adapter.rename_entry("/dir/old.txt", "/dir/new.txt"))
            .unwrap();

        assert!(
            rt.block_on(adapter.cache.get_attr("/dir/old.txt"))
                .is_none(),
            "source attr cache should be invalidated"
        );
        assert!(
            rt.block_on(adapter.cache.get_children("/dir")).is_none(),
            "parent children cache should be invalidated"
        );

        // New path should be fetchable (from backend)
        let entry = rt.block_on(adapter.getattr("/dir/new.txt")).unwrap();
        assert_eq!(entry.name, "new.txt");
    }

    #[test]
    fn test_subscribe_receives_events() {
        let backend = MockBackend::new();
        let adapter = FuseAdapter::new(backend, &make_config());
        let mut rx = adapter.subscribe();

        adapter.emit(MountEvent::CacheHit {
            path: PathBuf::from("/test"),
        });
        let event = rx.try_recv().unwrap();
        match event {
            MountEvent::CacheHit { path } => assert_eq!(path, PathBuf::from("/test")),
            _ => panic!("Expected CacheHit"),
        }
    }
}
