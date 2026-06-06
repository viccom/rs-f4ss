use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::prefetch::ReadPattern;

pub const MAX_BUFFER_SIZE: usize = 2 * 1024 * 1024 * 1024; // 2 GiB

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAtError {
    InvalidHandle,
    TooLarge,
}

/// State tracked for an open file handle.
pub struct OpenFile {
    pub path: String,
    pub dirty: bool,
    /// Accumulated write buffer. Grows to fit writes at any offset.
    pub buffer: Vec<u8>,
    /// Read-ahead cache: (data, start_offset). Populated on first read,
    /// subsequent reads within the cached range avoid HTTP requests.
    pub read_cache: Option<(Vec<u8>, u64)>,
    /// Sequential read pattern tracker.
    pub read_pattern: ReadPattern,
}

/// Thread-safe file handle table.
/// Maps u64 handles to open file state.
#[derive(Default)]
pub struct HandleTable {
    next_fh: AtomicU64,
    files: Mutex<HashMap<u64, OpenFile>>,
}

impl HandleTable {
    pub fn new() -> Self {
        Self {
            next_fh: AtomicU64::new(1),
            files: Mutex::new(HashMap::new()),
        }
    }

    /// Allocate a new file handle for the given path.
    pub fn allocate(&self, path: String) -> u64 {
        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.files.lock().expect("handle table lock").insert(
            fh,
            OpenFile {
                path,
                dirty: false,
                buffer: Vec::new(),
                read_cache: None,
                read_pattern: ReadPattern::new(),
            },
        );
        fh
    }

    /// Get the path associated with a file handle.
    pub fn get_path(&self, fh: u64) -> Option<String> {
        self.files
            .lock()
            .expect("handle table lock")
            .get(&fh)
            .map(|f| f.path.clone())
    }

    /// Write data to a file handle's buffer at the given offset, marking it dirty.
    /// The buffer grows to accommodate the write.
    pub fn write_at(&self, fh: u64, offset: u64, data: &[u8]) -> Result<(), WriteAtError> {
        let mut files = self.files.lock().expect("handle table lock");
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
            file.read_cache = None;
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
        let mut files = self.files.lock().expect("handle table lock");
        if let Some(file) = files.get_mut(&fh) {
            file.dirty = true;
            file.read_cache = None;
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
        let mut files = self.files.lock().expect("handle table lock");
        if let Some(file) = files.get_mut(&fh) {
            if !file.dirty && file.buffer.is_empty() {
                file.buffer = data;
                file.read_cache = None;
            }
            Ok(())
        } else {
            Err(WriteAtError::InvalidHandle)
        }
    }

    /// Take the dirty data from a file handle, resetting the dirty flag.
    /// Returns `(path, buffer)` if dirty, `None` if not dirty or handle missing.
    /// Lock is released before the caller does I/O.
    pub fn take_dirty(&self, fh: u64) -> Option<(String, Vec<u8>)> {
        let mut files = self.files.lock().expect("handle table lock");
        let file = files.get_mut(&fh)?;
        if !file.dirty {
            return None;
        }
        file.dirty = false;
        Some((file.path.clone(), std::mem::take(&mut file.buffer)))
    }

    /// Restore dirty data to a file handle after a failed write.
    pub fn restore_dirty(&self, fh: u64, buffer: Vec<u8>) {
        let mut files = self.files.lock().expect("handle table lock");
        if let Some(file) = files.get_mut(&fh) {
            file.dirty = true;
            file.buffer = buffer;
        }
    }

    /// Clone the current dirty data without changing handle state.
    pub fn peek_dirty(&self, fh: u64) -> Option<(String, Vec<u8>)> {
        let files = self.files.lock().expect("handle table lock");
        let file = files.get(&fh)?;
        if !file.dirty {
            return None;
        }
        Some((file.path.clone(), file.buffer.clone()))
    }

    /// Get the current dirty buffer length, if the handle has unflushed changes.
    pub fn dirty_len(&self, fh: u64) -> Option<usize> {
        let files = self.files.lock().expect("handle table lock");
        let file = files.get(&fh)?;
        if !file.dirty {
            return None;
        }
        Some(file.buffer.len())
    }

    /// Read directly from the dirty write buffer for same-handle visibility.
    pub fn read_from_dirty(&self, fh: u64, offset: u64, size: u32) -> Option<Vec<u8>> {
        let files = self.files.lock().expect("handle table lock");
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
        self.files
            .lock()
            .expect("handle table lock")
            .keys()
            .copied()
            .collect()
    }

    /// Remove a file handle, returning its state (for release).
    pub fn remove(&self, fh: u64) -> Option<OpenFile> {
        self.files.lock().expect("handle table lock").remove(&fh)
    }

    /// Update read pattern for a handle, returns true if sequential.
    pub fn update_read_pattern(&self, fh: u64, offset: u64, size: u32) -> bool {
        let mut files = self.files.lock().expect("handle table lock");
        if let Some(file) = files.get_mut(&fh) {
            file.read_pattern.update(offset, size)
        } else {
            false
        }
    }

    /// Get read pattern info (is_first_read).
    pub fn is_first_read(&self, fh: u64) -> bool {
        let files = self.files.lock().expect("handle table lock");
        files
            .get(&fh)
            .map(|f| f.read_pattern.is_first_read())
            .unwrap_or(false)
    }

    /// Get current read cache bounds: (start_offset, data_len).
    pub fn get_cache_info(&self, fh: u64) -> Option<(u64, usize)> {
        let files = self.files.lock().expect("handle table lock");
        files.get(&fh).and_then(|f| {
            f.read_cache
                .as_ref()
                .map(|(data, offset)| (*offset, data.len()))
        })
    }

    /// Get (last_read_end, is_sequential) for a handle.
    pub fn get_read_state(&self, fh: u64) -> Option<(u64, bool)> {
        let files = self.files.lock().expect("handle table lock");
        files
            .get(&fh)
            .map(|f| (f.read_pattern.last_read_end, f.read_pattern.is_sequential))
    }

    /// Try to serve a read from the handle's read cache.
    /// Returns Some(data) if cache hit, None if cache miss.
    pub fn read_from_cache(&self, fh: u64, offset: u64, size: u32) -> Option<Vec<u8>> {
        let files = self.files.lock().expect("handle table lock");
        let file = files.get(&fh)?;
        let (cache_data, cache_offset) = file.read_cache.as_ref()?;
        let cache_end = *cache_offset + cache_data.len() as u64;
        if offset >= *cache_offset && offset.saturating_add(size as u64) <= cache_end {
            let start = (offset - *cache_offset) as usize;
            let end = start + size as usize;
            Some(cache_data[start..end].to_vec())
        } else {
            None
        }
    }

    /// Store a read-ahead cache for a handle.
    pub fn set_read_cache(&self, fh: u64, data: Vec<u8>, offset: u64) {
        let mut files = self.files.lock().expect("handle table lock");
        if let Some(file) = files.get_mut(&fh) {
            file.read_cache = Some((data, offset));
        }
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
        assert_eq!(table.get_path(fh).unwrap(), "/test");
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
        assert_eq!(path, "/test");
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
        assert_eq!(path, "/peek.txt");
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
        assert_eq!(path, "/replace.txt");
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
