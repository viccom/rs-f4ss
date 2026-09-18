use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use crate::error::BackendError;
use crate::window::{read_at, ReadWindow, WindowState, DEFAULT_READ_WINDOW};

pub const MAX_BUFFER_SIZE: usize = 2 * 1024 * 1024 * 1024; // 2 GiB

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAtError {
    InvalidHandle,
    TooLarge,
}

/// State tracked for an open file handle.
pub struct OpenFile {
    pub path: Arc<str>,
    pub dirty: bool,
    /// Accumulated write buffer. Grows to fit writes at any offset.
    pub buffer: Vec<u8>,
    /// 锚定窗口读状态（替代 read_cache + read_pattern）。
    pub window: WindowState,
}

/// Thread-safe file handle table.
/// Maps u64 handles to open file state.
/// Uses RwLock so concurrent reads don't block each other.
#[derive(Default)]
pub struct HandleTable {
    next_fh: AtomicU64,
    files: RwLock<HashMap<u64, OpenFile>>,
}

impl HandleTable {
    pub fn new() -> Self {
        Self {
            next_fh: AtomicU64::new(1),
            files: RwLock::new(HashMap::new()),
        }
    }

    fn read_table(&self) -> std::sync::RwLockReadGuard<'_, HashMap<u64, OpenFile>> {
        self.files.read().unwrap_or_else(|e| {
            tracing::warn!("Recovering from poisoned handle table lock");
            e.into_inner()
        })
    }

    fn write_table(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<u64, OpenFile>> {
        self.files.write().unwrap_or_else(|e| {
            tracing::warn!("Recovering from poisoned handle table lock");
            e.into_inner()
        })
    }

    /// Allocate a new file handle for the given path.
    pub fn allocate(&self, path: String) -> u64 {
        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.write_table().insert(
            fh,
            OpenFile {
                path: Arc::from(path),
                dirty: false,
                buffer: Vec::new(),
                window: WindowState::new(DEFAULT_READ_WINDOW),
            },
        );
        fh
    }

    /// Get the path associated with a file handle.
    /// Returns Arc<str> for O(1) clone — avoids allocation on every read.
    pub fn get_path(&self, fh: u64) -> Option<Arc<str>> {
        self.read_table().get(&fh).map(|f| f.path.clone())
    }

    /// Write data to a file handle's buffer at the given offset, marking it dirty.
    /// The buffer grows to accommodate the write.
    pub fn write_at(&self, fh: u64, offset: u64, data: &[u8]) -> Result<(), WriteAtError> {
        let mut files = self.write_table();
        if let Some(file) = files.get_mut(&fh) {
            let start = match usize::try_from(offset) {
                Ok(s) => s,
                Err(_) => return Err(WriteAtError::TooLarge),
            };
            let end = start
                .checked_add(data.len())
                .ok_or(WriteAtError::TooLarge)?;
            if end > MAX_BUFFER_SIZE {
                tracing::error!("write_at: buffer size {end} exceeds {MAX_BUFFER_SIZE}, rejecting");
                return Err(WriteAtError::TooLarge);
            }
            file.dirty = true;
            file.window.window = None; // 写后窗口失效，read-your-own-writes 走 dirty 臂
            if end > file.buffer.len() {
                file.buffer.resize(end, 0);
            }
            file.buffer[start..end].copy_from_slice(data);
            Ok(())
        } else {
            Err(WriteAtError::InvalidHandle)
        }
    }

    /// Replace a handle's entire buffered contents and mark it dirty.
    pub fn replace_contents(&self, fh: u64, data: Vec<u8>) -> Result<(), WriteAtError> {
        if data.len() > MAX_BUFFER_SIZE {
            tracing::error!(
                "replace_contents: buffer size {} exceeds {}, rejecting",
                data.len(),
                MAX_BUFFER_SIZE
            );
            return Err(WriteAtError::TooLarge);
        }
        let mut files = self.write_table();
        if let Some(file) = files.get_mut(&fh) {
            file.dirty = true;
            file.window.window = None;
            file.buffer = data;
            Ok(())
        } else {
            Err(WriteAtError::InvalidHandle)
        }
    }

    /// Hydrate a handle with a clean full-file snapshot before the first modification.
    pub fn hydrate_contents(&self, fh: u64, data: Vec<u8>) -> Result<(), WriteAtError> {
        if data.len() > MAX_BUFFER_SIZE {
            tracing::error!(
                "hydrate_contents: buffer size {} exceeds {}, rejecting",
                data.len(),
                MAX_BUFFER_SIZE
            );
            return Err(WriteAtError::TooLarge);
        }
        let mut files = self.write_table();
        if let Some(file) = files.get_mut(&fh) {
            if !file.dirty && file.buffer.is_empty() {
                file.buffer = data;
                file.window.window = None;
            }
            Ok(())
        } else {
            Err(WriteAtError::InvalidHandle)
        }
    }

    /// Take the dirty data from a file handle, resetting the dirty flag.
    /// Returns `(path, buffer)` if dirty, `None` if not dirty or handle missing.
    /// Lock is released before the caller does I/O.
    pub fn take_dirty(&self, fh: u64) -> Option<(Arc<str>, Vec<u8>)> {
        let mut files = self.write_table();
        let file = files.get_mut(&fh)?;
        if !file.dirty {
            return None;
        }
        file.dirty = false;
        Some((file.path.clone(), std::mem::take(&mut file.buffer)))
    }

    /// Restore dirty data to a file handle after a failed write.
    pub fn restore_dirty(&self, fh: u64, buffer: Vec<u8>) {
        let mut files = self.write_table();
        if let Some(file) = files.get_mut(&fh) {
            file.dirty = true;
            file.buffer = buffer;
        }
    }

    /// Length of the content we can currently serve with confidence:
    /// dirty buffer length while unflushed writes exist, otherwise the
    /// hydrated snapshot length (committed backend content). Used to clamp
    /// constrained (Cache Manager) writes that carry page-tail padding.
    pub fn valid_data_len(&self, fh: u64) -> Option<usize> {
        let files = self.read_table();
        let file = files.get(&fh)?;
        if file.dirty {
            Some(file.buffer.len())
        } else if file.buffer.is_empty() {
            None
        } else {
            Some(file.buffer.len())
        }
    }

    /// Clone the current dirty data without changing handle state.
    pub fn peek_dirty(&self, fh: u64) -> Option<(Arc<str>, Vec<u8>)> {
        let files = self.read_table();
        let file = files.get(&fh)?;
        if !file.dirty {
            return None;
        }
        Some((file.path.clone(), file.buffer.clone()))
    }

    /// Get the current dirty buffer length, if the handle has unflushed changes.
    pub fn dirty_len(&self, fh: u64) -> Option<usize> {
        let files = self.read_table();
        let file = files.get(&fh)?;
        if !file.dirty {
            return None;
        }
        Some(file.buffer.len())
    }

    /// Read directly from the dirty write buffer for same-handle visibility.
    pub fn read_from_dirty(&self, fh: u64, offset: u64, size: u32) -> Option<Vec<u8>> {
        let files = self.read_table();
        let file = files.get(&fh)?;
        if !file.dirty {
            return None;
        }
        let start = usize::try_from(offset).ok()?;
        if start >= file.buffer.len() {
            return Some(Vec::new());
        }
        let end = start.saturating_add(size as usize).min(file.buffer.len());
        Some(file.buffer[start..end].to_vec())
    }

    /// Return all active file handle IDs.
    pub fn all_handles(&self) -> Vec<u64> {
        self.read_table().keys().copied().collect()
    }

    /// Remove a file handle, returning its state (for release).
    pub fn remove(&self, fh: u64) -> Option<OpenFile> {
        self.write_table().remove(&fh)
    }

    /// Adopt a grace-parked window into a freshly opened read handle
    /// (mount open wiring); the handle's window state is otherwise fresh.
    pub fn adopt_window(&self, fh: u64, w: ReadWindow) {
        let mut files = self.write_table();
        if let Some(file) = files.get_mut(&fh) {
            file.window.window = Some(w);
        }
    }

    /// Serve a read through the handle's anchored window. The table write
    /// lock is held across the fetch await, serializing reads on the same
    /// handle (cydrive K41 semantics); the lock never leaks to the caller.
    // 持锁跨 await 是本方法的契约（窗口状态与 fetch 的原子性），非事故。
    #[allow(clippy::await_holding_lock)]
    pub async fn read_window<F, Fut>(
        &self,
        fh: u64,
        known_size: Option<u64>,
        offset: u64,
        size: u32,
        fetch: F,
    ) -> Result<Vec<u8>, BackendError>
    where
        F: Fn(u64, u32) -> Fut,
        Fut: std::future::Future<Output = Result<Vec<u8>, BackendError>>,
    {
        let mut files = self.write_table();
        let file = files
            .get_mut(&fh)
            .ok_or_else(|| BackendError::NotFound("Invalid file handle".into()))?;
        file.window.size = known_size;
        read_at(&mut file.window, offset, size, fetch).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_nonzero() {
        let table = HandleTable::new();
        let fh = table.allocate("/test".to_string());
        assert!(fh > 0);
    }

    #[test]
    fn allocate_has_fresh_window_state() {
        let t = HandleTable::new();
        let fh = t.allocate("/a".into());
        let table = t.read_table();
        let f = table.get(&fh).unwrap();
        assert!(f.window.window.is_none());
        assert!(f.window.size.is_none());
    }

    #[test]
    fn test_allocate_unique() {
        let table = HandleTable::new();
        let a = table.allocate("/a".to_string());
        let b = table.allocate("/b".to_string());
        let c = table.allocate("/c".to_string());
        assert!(a != b && b != c && a != c);
    }

    #[test]
    fn test_get_after_alloc() {
        let table = HandleTable::new();
        let fh = table.allocate("/test".to_string());
        assert_eq!(&*table.get_path(fh).unwrap(), "/test");
    }

    #[test]
    fn test_get_after_remove() {
        let table = HandleTable::new();
        let fh = table.allocate("/test".to_string());
        table.remove(fh);
        assert!(table.get_path(fh).is_none());
    }

    #[test]
    fn test_write_buffer() {
        let table = HandleTable::new();
        let fh = table.allocate("/test".to_string());
        assert_eq!(table.write_at(fh, 0, b"hello"), Ok(()));

        let (path, buf) = table.take_dirty(fh).unwrap();
        assert_eq!(&*path, "/test");
        assert_eq!(buf, b"hello");

        // After take_dirty, not dirty anymore
        assert!(table.take_dirty(fh).is_none());
    }

    #[test]
    fn test_write_at_offset() {
        let table = HandleTable::new();
        let fh = table.allocate("/test".to_string());

        // Write two chunks like dd would
        assert_eq!(table.write_at(fh, 0, b"AAAA"), Ok(()));
        assert_eq!(table.write_at(fh, 4, b"BBBB"), Ok(()));

        let (_, buf) = table.take_dirty(fh).unwrap();
        assert_eq!(buf, b"AAAABBBB");
    }

    #[test]
    fn test_write_at_nonzero_offset() {
        let table = HandleTable::new();
        let fh = table.allocate("/test".to_string());

        // Write with gap — zeros fill the gap
        assert_eq!(table.write_at(fh, 3, b"XYZ"), Ok(()));

        let (_, buf) = table.take_dirty(fh).unwrap();
        assert_eq!(buf.len(), 6);
        assert_eq!(&buf[3..6], b"XYZ");
    }

    #[test]
    fn test_write_at_invalid_handle_reports_specific_error() {
        let table = HandleTable::new();
        assert_eq!(
            table.write_at(9999, 0, b"hello"),
            Err(WriteAtError::InvalidHandle)
        );
    }

    #[test]
    fn test_write_at_too_large_reports_specific_error() {
        let table = HandleTable::new();
        let fh = table.allocate("/huge.bin".to_string());
        assert_eq!(
            table.write_at(fh, 2u64 * 1024 * 1024 * 1024, b"x"),
            Err(WriteAtError::TooLarge)
        );
    }

    #[test]
    fn test_peek_dirty_does_not_clear_buffer() {
        let table = HandleTable::new();
        let fh = table.allocate("/peek.txt".to_string());
        assert_eq!(table.write_at(fh, 0, b"peek"), Ok(()));

        let (path, buf) = table.peek_dirty(fh).unwrap();
        assert_eq!(&*path, "/peek.txt");
        assert_eq!(buf, b"peek");

        let (_, buf_after) = table.take_dirty(fh).unwrap();
        assert_eq!(buf_after, b"peek");
    }

    #[test]
    fn test_replace_contents_marks_handle_dirty() {
        let table = HandleTable::new();
        let fh = table.allocate("/replace.txt".to_string());
        assert_eq!(table.replace_contents(fh, b"abc".to_vec()), Ok(()));

        let (path, buf) = table.take_dirty(fh).unwrap();
        assert_eq!(&*path, "/replace.txt");
        assert_eq!(buf, b"abc");
    }

    #[test]
    fn test_read_from_dirty_uses_unflushed_buffer() {
        let table = HandleTable::new();
        let fh = table.allocate("/dirty.txt".to_string());
        assert_eq!(table.replace_contents(fh, b"abcdef".to_vec()), Ok(()));

        assert_eq!(table.dirty_len(fh), Some(6));
        assert_eq!(table.read_from_dirty(fh, 2, 3), Some(b"cde".to_vec()));
        assert_eq!(table.read_from_dirty(fh, 99, 3), Some(Vec::new()));
    }
}
