//! Windows WinFsp implementation via `winfsp` crate.
//!
//! Only compiled on `target_os = "windows"`.
//!
//! Reference: blockframe-rs (crushr3sist/blockframe-rs) — working WinFsp filesystem.

#![cfg(target_os = "windows")]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use winfsp::filesystem::{
    DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo, VolumeInfo,
    WideNameInfo,
};
use winfsp::host::{FileSystemHost, VolumeParams};
use winfsp::{FspError, U16CStr, U16CString};
use winfsp_sys::{FspCleanupDelete, FILE_ACCESS_RIGHTS, FILE_FLAGS_AND_ATTRIBUTES};

use crate::backend::StorageBackend;
use crate::error::MountError;
use crate::handle::{WriteAtError, MAX_BUFFER_SIZE};
use crate::mount::{FuseAdapter, MountConfig, MountEvent, MountStatus};

// ---------------------------------------------------------------------------
// Error conversion
// ---------------------------------------------------------------------------

fn mount_err_to_fsp(err: MountError) -> FspError {
    match &err {
        MountError::Backend(be) => {
            use std::io::ErrorKind;
            let kind = match be {
                crate::error::BackendError::NotFound(_) => ErrorKind::NotFound,
                crate::error::BackendError::PermissionDenied(_) => ErrorKind::PermissionDenied,
                crate::error::BackendError::ReadOnly => ErrorKind::PermissionDenied,
                crate::error::BackendError::InvalidPath(_) => ErrorKind::InvalidInput,
                _ => ErrorKind::Other,
            };
            FspError::IO(kind)
        }
        _ => FspError::IO(std::io::ErrorKind::Other),
    }
}

fn fsp<T>(result: std::result::Result<T, MountError>) -> std::result::Result<T, FspError> {
    result.map_err(|e| {
        tracing::debug!("backend error: {e}");
        mount_err_to_fsp(e)
    })
}

// ---------------------------------------------------------------------------
// FileContext
// ---------------------------------------------------------------------------

pub struct FileContext {
    path: String,
    is_dir: bool,
    delete_on_close: AtomicBool,
    delete_completed: AtomicBool,
    placeholder_state: AtomicU8,
    rollback_placeholder: AtomicBool,
    handle: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseAction {
    Release,
    Discard,
    RollbackPlaceholder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlaceholderState {
    None = 0,
    Placeholder = 1,
    Materialized = 2,
}

impl FileContext {
    fn new(
        path: String,
        is_dir: bool,
        handle: Option<u64>,
        placeholder_state: PlaceholderState,
        delete_on_close: bool,
    ) -> Self {
        Self {
            path,
            is_dir,
            delete_on_close: AtomicBool::new(delete_on_close),
            delete_completed: AtomicBool::new(false),
            placeholder_state: AtomicU8::new(placeholder_state as u8),
            rollback_placeholder: AtomicBool::new(false),
            handle,
        }
    }

    #[cfg(test)]
    fn new_test(path: &str, is_dir: bool, placeholder: bool) -> Self {
        Self::new(
            path.to_string(),
            is_dir,
            Some(1),
            if placeholder {
                PlaceholderState::Placeholder
            } else {
                PlaceholderState::None
            },
            false,
        )
    }

    fn placeholder_state(&self) -> PlaceholderState {
        match self.placeholder_state.load(Ordering::Relaxed) {
            1 => PlaceholderState::Placeholder,
            2 => PlaceholderState::Materialized,
            _ => PlaceholderState::None,
        }
    }

    fn delete_requested(&self) -> bool {
        self.delete_on_close.load(Ordering::Relaxed)
    }

    fn update_delete_requested(&self, delete_file: bool) {
        self.delete_on_close.store(delete_file, Ordering::Relaxed);
        if !delete_file {
            self.delete_completed.store(false, Ordering::Relaxed);
        }
    }

    fn wants_delete_in_cleanup(&self, flags: u32) -> bool {
        self.delete_requested() || (flags & FspCleanupDelete as u32) != 0
    }

    fn record_cleanup_result(&self, flags: u32, succeeded: bool) {
        if !self.wants_delete_in_cleanup(flags) {
            return;
        }
        self.delete_on_close.store(true, Ordering::Relaxed);
        self.delete_completed.store(succeeded, Ordering::Relaxed);
    }

    fn mark_materialized(&self) {
        if self.is_dir {
            return;
        }
        self.placeholder_state
            .store(PlaceholderState::Materialized as u8, Ordering::Relaxed);
        self.rollback_placeholder.store(false, Ordering::Relaxed);
    }

    fn record_write_failure(&self) {
        if self.placeholder_state() == PlaceholderState::Placeholder {
            self.rollback_placeholder.store(true, Ordering::Relaxed);
        }
    }

    fn close_action(&self) -> CloseAction {
        if self.delete_completed.load(Ordering::Relaxed) {
            CloseAction::Discard
        } else if self.rollback_placeholder.load(Ordering::Relaxed)
            && self.placeholder_state() == PlaceholderState::Placeholder
        {
            CloseAction::RollbackPlaceholder
        } else {
            CloseAction::Release
        }
    }
}

// ---------------------------------------------------------------------------
// WinFspAdapter
// ---------------------------------------------------------------------------

pub struct WinFspAdapter<B: StorageBackend> {
    inner: Arc<FuseAdapter<B>>,
    dispatcher_alive: Arc<AtomicBool>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn u16_to_path(s: &U16CStr) -> std::result::Result<String, FspError> {
    let utf8 = match s.to_string() {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!("u16_to_path: invalid UTF-16 in filename: {e}");
            return Err(FspError::IO(std::io::ErrorKind::InvalidData));
        }
    };
    let path = utf8.replace('\\', "/");
    if path.starts_with('/') {
        Ok(path)
    } else {
        Ok(format!("/{path}"))
    }
}

/// Fill all FileInfo fields (blockframe-rs pattern — no garbage values).
fn set_file_info_from_entry(entry: &crate::backend::Entry, info: &mut FileInfo) {
    info.file_attributes = if entry.dir { 0x10 } else { 0x20 };
    info.reparse_tag = 0;
    info.allocation_size = if entry.size > 0 {
        (entry.size + 4095) & !4095
    } else {
        0
    };
    info.file_size = entry.size;
    info.creation_time = 0;
    info.last_access_time = 0;
    info.last_write_time = 0;
    info.change_time = 0;
    info.index_number = 0;
    info.hard_links = 1;
    info.ea_size = 0;

    if let Ok(dur) = entry.mtime.duration_since(std::time::UNIX_EPOCH) {
        let epoch_diff: u64 = 116_444_736_000_000_000;
        let ft = dur.as_nanos() as u64 / 100 + epoch_diff;
        info.creation_time = ft;
        info.last_access_time = ft;
        info.last_write_time = ft;
        info.change_time = ft;
    }
}

const FILE_DELETE_ON_CLOSE_OPTION: u32 = 0x0000_1000;

fn delete_on_close_requested(create_options: u32) -> bool {
    (create_options & FILE_DELETE_ON_CLOSE_OPTION) != 0
}

fn write_error_to_fsp(err: WriteAtError) -> FspError {
    match err {
        WriteAtError::InvalidHandle => FspError::IO(std::io::ErrorKind::InvalidInput),
        WriteAtError::TooLarge => FspError::IO(std::io::ErrorKind::FileTooLarge),
    }
}

impl<B: StorageBackend + 'static> WinFspAdapter<B> {
    fn emit_error(&self, msg: String) {
        tracing::error!("{msg}");
        self.inner.emit(MountEvent::Error { error: msg });
    }

    /// Max file size for in-memory hydration (256 MB).
    /// Files larger than this cannot be modified through the buffer path.
    const HYDRATE_MAX: u64 = 256 * 1024 * 1024;

    fn ensure_full_buffer(
        &self,
        context: &FileContext,
        fh: u64,
    ) -> std::result::Result<(), FspError> {
        if context.is_dir || self.inner.handles.dirty_len(fh).is_some() {
            return Ok(());
        }
        if context.placeholder_state() == PlaceholderState::Placeholder {
            return Ok(());
        }

        let entry = fsp(self.inner.block_on(self.inner.getattr(&context.path)))?;
        if entry.size > Self::HYDRATE_MAX {
            return Err(FspError::IO(std::io::ErrorKind::FileTooLarge));
        }

        let data = if entry.size == 0 {
            Vec::new()
        } else {
            fsp(self.inner.block_on(async {
                self.inner
                    .backend
                    .read(&context.path, 0, entry.size as u32)
                    .await
                    .map_err(MountError::Backend)
            }))?
        };

        self.inner
            .handles
            .hydrate_contents(fh, data)
            .map_err(write_error_to_fsp)
    }

    fn rollback_placeholder(&self, path: &str) {
        for attempt in 1..=3 {
            match self.inner.block_on(self.inner.unlink(path)) {
                Ok(()) => return,
                Err(e) if attempt < 3 => {
                    tracing::warn!(
                        "[close] placeholder rollback attempt {} failed for \"{}\": {e}",
                        attempt,
                        path
                    );
                    std::thread::sleep(Duration::from_millis(100 * attempt as u64));
                }
                Err(e) => {
                    self.emit_error(format!(
                        "close rollback failed for placeholder \"{}\": {e}",
                        path
                    ));
                    return;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// FileSystemContext
// ---------------------------------------------------------------------------

impl<B: StorageBackend + 'static> FileSystemContext for WinFspAdapter<B> {
    type FileContext = FileContext;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        _security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> std::result::Result<FileSecurity, FspError> {
        let path = u16_to_path(file_name)?;
        tracing::trace!("[gsbn] \"{path}\"");
        let entry = fsp(self.inner.block_on(self.inner.getattr(&path)))?;
        let attrs = if entry.dir { 0x10 } else { 0x20 };
        tracing::trace!("[gsbn] OK \"{path}\" attrs=0x{attrs:02X}");
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: 0,
            attributes: attrs,
        })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        granted_access: FILE_ACCESS_RIGHTS,
        file_info: &mut OpenFileInfo,
    ) -> std::result::Result<Self::FileContext, FspError> {
        let path = u16_to_path(file_name)?;
        tracing::trace!("[open] \"{path}\" create_opts=0x{create_options:08X}");
        let entry = fsp(self.inner.block_on(self.inner.getattr(&path)))?;
        set_file_info_from_entry(&entry, file_info.as_mut());

        if entry.dir {
            file_info.as_mut().allocation_size = 0;
            file_info.as_mut().file_size = 0;
        }

        let is_dir = entry.dir;
        let handle = if !is_dir {
            Some(fsp(self.inner.block_on(self.inner.open(&path, false)))?)
        } else {
            None
        };

        tracing::trace!(
            "[open] OK \"{path}\" is_dir={is_dir} size={} alloc={}",
            entry.size,
            if entry.dir {
                0
            } else {
                (entry.size + 4095) & !4095
            }
        );
        Ok(FileContext::new(
            path,
            is_dir,
            handle,
            PlaceholderState::None,
            delete_on_close_requested(create_options),
        ))
    }

    fn close(&self, context: Self::FileContext) {
        tracing::debug!("[close] \"{}\"", context.path);
        if let Some(fh) = context.handle {
            match context.close_action() {
                CloseAction::Discard => self.inner.discard_handle(fh),
                CloseAction::RollbackPlaceholder => {
                    self.rollback_placeholder(&context.path);
                    self.inner.discard_handle(fh);
                }
                CloseAction::Release => {
                    if let Err(e) = self.inner.block_on(self.inner.release(fh)) {
                        self.emit_error(format!(
                            "close release failed for \"{}\": {e}",
                            context.path
                        ));
                    }
                }
            }
        }
    }

    fn create(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        _granted_access: FILE_ACCESS_RIGHTS,
        _file_attributes: FILE_FLAGS_AND_ATTRIBUTES,
        _security_descriptor: Option<&[c_void]>,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        _extra_buffer_is_reparse_point: bool,
        file_info: &mut OpenFileInfo,
    ) -> std::result::Result<Self::FileContext, FspError> {
        let path = u16_to_path(file_name)?;
        let is_dir = (create_options & 0x0000_0001) != 0;
        tracing::debug!("[create] \"{path}\" is_dir={is_dir}");

        if is_dir {
            fsp(self.inner.block_on(self.inner.mkdir(&path)))?;
        } else {
            // Write empty body directly (1 PUT) + cache invalidation.
            // Previously: open→flush(PUT)→release→open = 4 HTTP requests.
            fsp(self.inner.block_on(async {
                self.inner
                    .backend
                    .write(&path, &[])
                    .await
                    .map_err(MountError::Backend)?;
                self.inner.cache.invalidate(&path).await;
                self.inner.cache.invalidate_parent(&path).await;
                Ok::<(), MountError>(())
            }))?;
        }

        let entry = fsp(self.inner.block_on(self.inner.getattr(&path)))?;
        set_file_info_from_entry(&entry, file_info.as_mut());

        let handle = if !is_dir {
            Some(fsp(self.inner.block_on(self.inner.open(&path, false)))?)
        } else {
            None
        };

        Ok(FileContext::new(
            path,
            is_dir,
            handle,
            if is_dir {
                PlaceholderState::None
            } else {
                PlaceholderState::Placeholder
            },
            delete_on_close_requested(create_options),
        ))
    }

    fn cleanup(&self, context: &Self::FileContext, _file_name: Option<&U16CStr>, flags: u32) {
        tracing::debug!("[cleanup] \"{}\"", context.path);
        if !context.wants_delete_in_cleanup(flags) {
            return;
        }
        let result = if context.is_dir {
            self.inner.block_on(self.inner.rmdir(&context.path))
        } else {
            self.inner.block_on(self.inner.unlink(&context.path))
        };
        context.record_cleanup_result(flags, result.is_ok());
        if let Err(e) = result {
            let msg = format!("cleanup delete failed for {}: {e}", context.path);
            tracing::warn!("{msg}");
            self.inner.emit(MountEvent::Error { error: msg });
        }
    }

    fn get_file_info(
        &self,
        context: &Self::FileContext,
        file_info: &mut FileInfo,
    ) -> std::result::Result<(), FspError> {
        tracing::trace!("[get_file_info] \"{}\"", context.path);
        let entry = fsp(self.inner.block_on(self.inner.getattr(&context.path)))?;
        set_file_info_from_entry(&entry, file_info);
        if let Some(fh) = context.handle {
            if let Some(len) = self.inner.handles.dirty_len(fh) {
                file_info.file_size = len as u64;
                file_info.allocation_size = if len > 0 {
                    ((len as u64) + 4095) & !4095
                } else {
                    0
                };
            }
        }
        Ok(())
    }

    fn read_directory(
        &self,
        context: &Self::FileContext,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> std::result::Result<u32, FspError> {
        tracing::debug!("[read_directory] path=\"{}\"", context.path);

        if !context.is_dir {
            return Err(FspError::NTSTATUS(-1073741808)); // STATUS_NOT_A_DIRECTORY
        }

        let dir_entry = match fsp(self.inner.block_on(self.inner.getattr(&context.path))) {
            Ok(e) => e,
            Err(e) => return Err(e),
        };

        let entries = match fsp(self.inner.block_on(self.inner.readdir(&context.path))) {
            Ok(e) => e,
            Err(e) => return Err(e),
        };
        tracing::debug!("[read_directory] {} entries", entries.len());

        let mut cursor = 0u32;

        if marker.is_none() {
            let mut dot_info: DirInfo = DirInfo::new();
            let dot_name = U16CString::from_str(".").unwrap();
            dot_info.set_name_raw(dot_name.as_slice()).unwrap();
            set_file_info_from_entry(&dir_entry, dot_info.file_info_mut());
            if !dot_info.append_to_buffer(buffer, &mut cursor) {
                DirInfo::<255>::finalize_buffer(buffer, &mut cursor);
                return Ok(cursor);
            }

            let mut dotdot_info: DirInfo = DirInfo::new();
            let dotdot_name = U16CString::from_str("..").unwrap();
            dotdot_info.set_name_raw(dotdot_name.as_slice()).unwrap();
            set_file_info_from_entry(&dir_entry, dotdot_info.file_info_mut());
            if !dotdot_info.append_to_buffer(buffer, &mut cursor) {
                DirInfo::<255>::finalize_buffer(buffer, &mut cursor);
                return Ok(cursor);
            }
        }

        let mut skip = !marker.is_none();
        for (_i, entry) in entries.iter().enumerate() {
            if skip {
                if let Some(marker_name) = marker.inner_as_cstr() {
                    if let Ok(entry_name) = U16CString::from_str(&entry.name) {
                        if entry_name.as_slice_with_nul() == marker_name.as_slice_with_nul() {
                            skip = false;
                        }
                    }
                }
                continue;
            }

            let mut dir_info: DirInfo = DirInfo::new();
            let name_u16 = match U16CString::from_str(&entry.name) {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!("name conversion failed for \"{}\": {e:?}", entry.name);
                    continue;
                }
            };

            if let Err(e) = dir_info.set_name_raw(name_u16.as_slice()) {
                tracing::warn!("set_name_raw failed for \"{}\": {e:?}", entry.name);
                continue;
            }

            set_file_info_from_entry(entry, dir_info.file_info_mut());

            if !dir_info.append_to_buffer(buffer, &mut cursor) {
                break;
            }
        }

        DirInfo::<255>::finalize_buffer(buffer, &mut cursor);
        Ok(cursor)
    }

    fn rename(
        &self,
        _context: &Self::FileContext,
        file_name: &U16CStr,
        new_file_name: &U16CStr,
        _replace_if_exists: bool,
    ) -> std::result::Result<(), FspError> {
        let old = u16_to_path(file_name)?;
        let new = u16_to_path(new_file_name)?;
        tracing::debug!("[rename] \"{old}\" → \"{new}\"");
        fsp(self.inner.block_on(self.inner.rename_entry(&old, &new)))?;
        Ok(())
    }

    fn set_delete(
        &self,
        context: &Self::FileContext,
        _file_name: &U16CStr,
        delete_file: bool,
    ) -> std::result::Result<(), FspError> {
        context.update_delete_requested(delete_file);
        Ok(())
    }

    fn read(
        &self,
        context: &Self::FileContext,
        buffer: &mut [u8],
        offset: u64,
    ) -> std::result::Result<u32, FspError> {
        tracing::trace!(
            "[read] \"{}\" offset={offset} size={}",
            context.path,
            buffer.len()
        );
        let fh = context
            .handle
            .ok_or_else(|| FspError::IO(std::io::ErrorKind::InvalidInput))?;
        if let Some(data) = self
            .inner
            .handles
            .read_from_dirty(fh, offset, buffer.len() as u32)
        {
            let n = data.len().min(buffer.len());
            buffer[..n].copy_from_slice(&data[..n]);
            tracing::trace!("[read] \"{}\" served {n} dirty bytes", context.path);
            return Ok(n as u32);
        }
        let data = fsp(self
            .inner
            .block_on(self.inner.read(fh, offset, buffer.len() as u32)))?;
        let n = data.len().min(buffer.len());
        buffer[..n].copy_from_slice(&data[..n]);
        tracing::trace!("[read] \"{}\" returned {n} bytes", context.path);
        Ok(n as u32)
    }

    fn write(
        &self,
        context: &Self::FileContext,
        buffer: &[u8],
        offset: u64,
        _write_to_eof: bool,
        _constrained_io: bool,
        file_info: &mut FileInfo,
    ) -> std::result::Result<u32, FspError> {
        let fh = context
            .handle
            .ok_or_else(|| FspError::IO(std::io::ErrorKind::InvalidInput))?;
        self.ensure_full_buffer(context, fh)?;
        if let Err(e) = fsp(self.inner.block_on(self.inner.write(fh, offset, buffer))) {
            context.record_write_failure();
            return Err(e);
        }
        if !buffer.is_empty() {
            context.mark_materialized();
        }
        // Update size locally instead of extra HTTP PROPFIND per write.
        let new_size = offset + buffer.len() as u64;
        if new_size > file_info.file_size {
            file_info.file_size = new_size;
            file_info.allocation_size = (new_size + 4095) & !4095;
        }
        Ok(buffer.len() as u32)
    }

    fn flush(
        &self,
        context: Option<&Self::FileContext>,
        file_info: &mut FileInfo,
    ) -> std::result::Result<(), FspError> {
        if let Some(ctx) = context {
            if let Some(fh) = ctx.handle {
                fsp(self.inner.block_on(self.inner.flush(fh)))?;
            }
            let entry = fsp(self.inner.block_on(self.inner.getattr(&ctx.path)))?;
            set_file_info_from_entry(&entry, file_info);
        }
        Ok(())
    }

    fn set_file_size(
        &self,
        context: &Self::FileContext,
        new_size: u64,
        _set_allocation_size: bool,
        file_info: &mut FileInfo,
    ) -> std::result::Result<(), FspError> {
        let fh = context
            .handle
            .ok_or_else(|| FspError::IO(std::io::ErrorKind::InvalidInput))?;
        self.ensure_full_buffer(context, fh)?;
        if new_size > Self::HYDRATE_MAX {
            context.record_write_failure();
            return Err(FspError::IO(std::io::ErrorKind::FileTooLarge));
        }

        let mut data = if let Some((_path, dirty)) = self.inner.handles.peek_dirty(fh) {
            dirty
        } else if new_size == 0 {
            Vec::new()
        } else {
            let read_len = new_size.min(u32::MAX as u64) as u32;
            match fsp(self.inner.block_on(async {
                self.inner
                    .backend
                    .read(&context.path, 0, read_len)
                    .await
                    .map_err(MountError::Backend)
            })) {
                Ok(data) => data,
                Err(e) => {
                    context.record_write_failure();
                    return Err(e);
                }
            }
        };
        data.resize(new_size as usize, 0);
        self.inner.handles.replace_contents(fh, data).map_err(|e| {
            context.record_write_failure();
            write_error_to_fsp(e)
        })?;
        context.mark_materialized();
        file_info.file_size = new_size;
        file_info.allocation_size = if new_size > 0 {
            (new_size + 4095) & !4095
        } else {
            0
        };
        Ok(())
    }

    fn get_volume_info(&self, out: &mut VolumeInfo) -> std::result::Result<(), FspError> {
        tracing::debug!("[get_volume_info]");
        let tb: u64 = 1024 * 1024 * 1024 * 1024;
        out.total_size = tb;
        out.free_size = tb;
        out.set_volume_label("rs-f4ss");
        Ok(())
    }

    fn dispatcher_stopped(&self, normally: bool) {
        tracing::info!("dispatcher_stopped(normally={normally})");
        self.dispatcher_alive.store(false, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Mount
// ---------------------------------------------------------------------------

const WINFSP_DLL: &str = "winfsp-x64.dll";

fn check_winfsp_driver() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let mut service_ok = false;
    let names = ["WinFsp", "WinFsp.Launcher"];
    for name in &names {
        if let Ok(o) = std::process::Command::new("sc")
            .args(["query", name])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let s = String::from_utf8_lossy(&o.stdout);
            if s.contains("RUNNING") {
                tracing::info!("WinFsp ({name}): RUNNING");
                service_ok = true;
                break;
            }
            if s.contains("STOPPED") {
                tracing::warn!("WinFsp ({name}): STOPPED");
                break;
            }
        }
    }
    if !service_ok {
        tracing::warn!("WinFsp service not detected. Install from https://winfsp.dev");
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    let local_dll = exe_dir.as_ref().map(|d| d.join(WINFSP_DLL));
    let system_dirs = [
        std::path::PathBuf::from(format!("C:\\Program Files\\WinFsp\\bin\\{WINFSP_DLL}")),
        std::path::PathBuf::from(format!(
            "C:\\Program Files (x86)\\WinFsp\\bin\\{WINFSP_DLL}"
        )),
        std::path::PathBuf::from(format!("C:\\Program Files\\WinFsp\\{WINFSP_DLL}")),
    ];

    if let Some(ref p) = local_dll {
        if p.exists() {
            tracing::info!("WinFsp DLL: {} (local)", p.display());
            return;
        }
    }

    for dir in &system_dirs {
        if dir.exists() {
            tracing::info!("WinFsp DLL: {} (system)", dir.display());
            return;
        }
    }

    tracing::warn!("{WINFSP_DLL} not found. Install WinFsp or copy DLL to exe directory.");
}

pub fn mount_windows<B: StorageBackend + 'static>(
    adapter: FuseAdapter<B>,
    config: &MountConfig,
    status: &std::sync::Mutex<MountStatus>,
    event_tx: &tokio::sync::broadcast::Sender<MountEvent>,
) -> std::result::Result<(), MountError> {
    check_winfsp_driver();

    tracing::info!("Initializing WinFsp...");
    let _init = winfsp::winfsp_init().map_err(|e| {
        let msg = format!(
            "WinFsp init failed: {e}\n\
             Possible causes:\n\
             - {WINFSP_DLL} is missing or incompatible\n\
             - Copy the correct {WINFSP_DLL} to the program directory\n\
             - Reinstall WinFsp from https://winfsp.dev"
        );
        MountError::FuseError(msg)
    })?;

    tracing::info!("Testing backend...");
    match adapter.block_on(adapter.getattr("/")) {
        Ok(e) => tracing::info!("Backend OK: root is_dir={}", e.dir),
        Err(e) => {
            tracing::error!("Backend FAILED: {e}");
            adapter
                .rt
                .shutdown_timeout(std::time::Duration::from_secs(2));
            return Err(e);
        }
    }

    tracing::info!("Testing readdir...");
    match adapter.block_on(adapter.readdir("/")) {
        Ok(entries) => tracing::info!("Readdir OK: {} entries", entries.len()),
        Err(e) => {
            tracing::error!("Readdir FAILED: {e}");
            adapter
                .rt
                .shutdown_timeout(std::time::Duration::from_secs(2));
            return Err(e);
        }
    }

    let dispatcher_alive = Arc::new(AtomicBool::new(true));
    let adapter_arc = Arc::new(adapter);
    let winfsp_adapter = WinFspAdapter {
        inner: adapter_arc.clone(),
        dispatcher_alive: dispatcher_alive.clone(),
    };

    let mut params = VolumeParams::new();
    params
        .filesystem_name("rs-f4ss")
        .volume_serial_number(0x2D0F5)
        .sector_size(512)
        .sectors_per_allocation_unit(1)
        .max_component_length(255)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        // FileInfoTimeout = 0xFFFFFFFF (infinite) enables the Windows Cache Manager.
        // This is the single most impactful setting for network filesystem performance:
        // - OS handles read-ahead and file data caching internally
        // - PotPlayer's repeated open/close probes are served from OS cache
        // - Sequential reads get OS-level prefetch instead of per-request callbacks
        // Same pattern used by rclone mount, SSHFS-Win, and WinFsp samples.
        .file_info_timeout(u32::MAX)
        .dir_info_timeout(u32::MAX)
        .volume_info_timeout(u32::MAX);
    if config.read_only {
        params.read_only_volume(true);
    }

    tracing::info!("Creating FileSystemHost...");
    let mut host = FileSystemHost::new(params, winfsp_adapter)
        .map_err(|e| MountError::FuseError(format!("host::new: {e}")))?;

    tracing::info!("Mounting at {}...", config.mountpoint.display());
    host.mount(&config.mountpoint)
        .map_err(|e| MountError::FuseError(format!("mount: {e}")))?;

    *status.lock().unwrap() = MountStatus::Mounted;
    if let Some(ref cb) = config.on_mount_ready {
        cb();
    }

    // Register stop signal via AtomicBool. The manager's stop() calls this
    // callback, which sets the flag. The polling loop detects it and calls
    // host.stop()/host.unmount() using the proper API — no raw pointer hacks.
    let stop_requested = Arc::new(AtomicBool::new(false));
    let stop_flag = stop_requested.clone();
    if let Some(ref setter) = config.on_set_unmount {
        setter(Arc::new(move || {
            stop_flag.store(true, Ordering::Release);
        }));
    }

    tracing::info!("Starting dispatcher...");
    host.start()
        .map_err(|e| MountError::FuseError(format!("start: {e}")))?;

    tracing::info!("Mount ready at {}", config.mountpoint.display());

    loop {
        if !dispatcher_alive.load(Ordering::Relaxed) {
            break;
        }
        if stop_requested.load(Ordering::Acquire) {
            tracing::info!("Stop requested, shutting down host...");
            adapter_arc.abort_all_prefetch();
            host.stop();
            host.unmount();
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    tracing::info!("Dispatcher stopped");
    event_tx.send(MountEvent::MountStopped).ok();
    *status.lock().unwrap() = MountStatus::Idle;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use winfsp_sys::FspCleanupDelete;

    #[test]
    fn test_u16_to_path_normalizes_backslashes() {
        let path = U16CString::from_str(r"folder\file.txt").unwrap();
        assert_eq!(u16_to_path(path.as_ucstr()).unwrap(), "/folder/file.txt");
    }

    #[test]
    fn test_u16_to_path_rejects_invalid_utf16() {
        let raw = [0xD800, 0];
        let path = U16CStr::from_slice(&raw).unwrap();
        assert!(u16_to_path(path).is_err());
    }

    #[test]
    fn test_cleanup_delete_flag_unifies_delete_on_close_state() {
        let ctx = FileContext::new_test("/delete.txt", false, false);
        assert_eq!(ctx.close_action(), CloseAction::Release);

        ctx.record_cleanup_result(FspCleanupDelete as u32, true);

        assert!(ctx.delete_requested());
        assert_eq!(ctx.close_action(), CloseAction::Discard);
    }

    #[test]
    fn test_set_file_size_materializes_placeholder() {
        let ctx = FileContext::new_test("/grow.bin", false, true);
        assert_eq!(ctx.close_action(), CloseAction::Release);

        ctx.mark_materialized();
        ctx.record_write_failure();

        assert_eq!(ctx.close_action(), CloseAction::Release);
    }

    #[test]
    fn test_failed_first_write_rolls_back_placeholder() {
        let ctx = FileContext::new_test("/placeholder.bin", false, true);
        ctx.record_write_failure();
        assert_eq!(ctx.close_action(), CloseAction::RollbackPlaceholder);
    }
}
