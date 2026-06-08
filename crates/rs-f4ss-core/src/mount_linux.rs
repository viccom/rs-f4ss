//! Linux FUSE implementation via `fuser` crate.
//!
//! This file contains the `fuser::Filesystem` trait implementation for `FuseAdapter`,
//! along with Linux-specific helpers. Only compiled on `target_os = "linux"`.

#[cfg(target_os = "linux")]
use std::ffi::OsStr;
#[cfg(target_os = "linux")]
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(target_os = "linux")]
use fuser::{
    BsdFileFlags, Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags,
    Generation, INodeNo, KernelConfig, LockOwner, MountOption, OpenFlags, RenameFlags, ReplyAttr,
    ReplyCreate, ReplyData, ReplyDirectory, ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyStatfs, ReplyWrite, Request, SessionACL,
};
#[cfg(target_os = "linux")]
use tracing::debug;

#[cfg(target_os = "linux")]
use crate::backend::{Entry, StorageBackend};
#[cfg(target_os = "linux")]
use crate::error::{BackendError, MountError};
#[cfg(target_os = "linux")]
use crate::inode::{NodeKind, ROOT_INODE};
#[cfg(target_os = "linux")]
use crate::mount::FuseAdapter;

// ---------------------------------------------------------------------------
// Error mapping (Linux-specific: BackendError → fuser::Errno)
// ---------------------------------------------------------------------------

/// Map `BackendError` to `fuser::Errno` for FUSE responses.
#[cfg(target_os = "linux")]
pub fn map_backend_error(err: &BackendError) -> Errno {
    match err {
        BackendError::NotFound(_) => Errno::ENOENT,
        BackendError::PermissionDenied(_) => Errno::EACCES,
        BackendError::ConnectionFailed(_) => Errno::EIO,
        BackendError::ReadOnly => Errno::EROFS,
        BackendError::ProtocolError(_) => Errno::EIO,
        BackendError::InvalidPath(_) => Errno::EINVAL,
        BackendError::NotSupported(_) => Errno::ENOTSUP,
        BackendError::Internal(_) => Errno::EIO,
    }
}

/// Map `MountError` to `fuser::Errno` (wraps `map_backend_error`).
#[cfg(target_os = "linux")]
pub fn map_mount_error(err: &MountError) -> Errno {
    match err {
        MountError::Backend(be) => map_backend_error(be),
        _ => Errno::EIO,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
const TTL: Duration = Duration::from_secs(60);

#[cfg(target_os = "linux")]
fn entry_to_file_attr(entry: &Entry, ino: u64) -> FileAttr {
    let kind = if entry.dir {
        FileType::Directory
    } else {
        FileType::RegularFile
    };
    let perm: u16 = if entry.dir { 0o755 } else { 0o644 };
    let nlink: u32 = if entry.dir { 2 } else { 1 };
    FileAttr {
        ino: INodeNo(ino),
        size: entry.size,
        blocks: entry.size.div_ceil(512),
        atime: entry.mtime,
        mtime: entry.mtime,
        ctime: entry.mtime,
        crtime: entry.mtime,
        kind,
        perm,
        nlink,
        uid: 0,
        gid: 0,
        rdev: 0,
        blksize: 1 << 20, // 1 MB — optimal for network FS I/O batching
        flags: 0,
    }
}

// ---------------------------------------------------------------------------
// fuser::Filesystem implementation
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
impl<B: StorageBackend> Filesystem for FuseAdapter<B> {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> std::io::Result<()> {
        debug!("FUSE init");
        config
            .add_capabilities(fuser::InitFlags::FUSE_ASYNC_READ)
            .ok();
        config
            .add_capabilities(fuser::InitFlags::FUSE_READDIRPLUS_AUTO)
            .ok();
        config.set_max_readahead(1024 * 1024).ok();
        Ok(())
    }

    fn destroy(&mut self) {
        debug!("FUSE destroy");
        for fh in self.handles.all_handles() {
            if self.handles.peek_dirty(fh).is_some() {
                if let Err(e) = self.block_on(self.flush(fh)) {
                    tracing::warn!("destroy: flush fh={fh} failed: {e}");
                }
            }
        }
        self.abort_all_prefetch();
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let parent_path = match self.inodes.get_path(parent.0) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let child_path = parent_path.join(name);
        let path_str = child_path.to_string_lossy().to_string();
        let inodes = self.inodes.clone();

        self.block_on(async move {
            match self.getattr(&path_str).await {
                Ok(entry) => {
                    let kind = if entry.dir {
                        NodeKind::Dir
                    } else {
                        NodeKind::File
                    };
                    let ino = inodes.get_or_insert(&child_path, kind);
                    reply.entry(&TTL, &entry_to_file_attr(&entry, ino), Generation(0));
                }
                Err(e) => reply.error(map_mount_error(&e)),
            }
        });
    }

    fn forget(&self, _req: &Request, ino: INodeNo, _nlookup: u64) {
        self.inodes.remove_by_inode(ino.0);
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let path = match self.inodes.get_path(ino.0) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let path_str = path.to_string_lossy().to_string();

        self.block_on(async move {
            match self.getattr(&path_str).await {
                Ok(entry) => reply.attr(&TTL, &entry_to_file_attr(&entry, ino.0)),
                Err(e) => reply.error(map_mount_error(&e)),
            }
        });
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<fuser::TimeOrNow>,
        _mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let path = match self.inodes.get_path(ino.0) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let path_str = path.to_string_lossy().to_string();
        let backend = self.backend.clone();

        self.block_on(async move {
            if let Some(new_size) = size {
                let result = if new_size == 0 {
                    backend.write(&path_str, &[]).await
                } else {
                    let new_size_u32 = match u32::try_from(new_size) {
                        Ok(s) => s,
                        Err(_) => {
                            tracing::warn!(
                                "setattr: truncating to {new_size} bytes exceeds max, rejecting"
                            );
                            reply.error(Errno::EFBIG);
                            return;
                        }
                    };
                    match backend.read(&path_str, 0, new_size_u32).await {
                        Ok(mut data) => {
                            data.resize(new_size as usize, 0);
                            backend.write(&path_str, &data).await
                        }
                        Err(e) => Err(e),
                    }
                };
                if let Err(e) = result {
                    reply.error(map_backend_error(&e));
                    return;
                }
                self.cache.invalidate(&path_str).await;
            }
            match self.getattr(&path_str).await {
                Ok(entry) => reply.attr(&TTL, &entry_to_file_attr(&entry, ino.0)),
                Err(e) => reply.error(map_mount_error(&e)),
            }
        });
    }

    fn open(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let path = match self.inodes.get_path(ino.0) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let path_str = path.to_string_lossy().to_string();
        let fh = self.handles.allocate(path_str.clone());

        // Use cached attr only — no extra stat. If the cache entry exists and
        // the file was recently stat'd (within moka TTL), the kernel page cache
        // is still valid so tell FUSE to keep it.
        let keep_cache = self
            .block_on(self.cache.get_attr(&path_str))
            .is_some_and(|c| c.entry.size > 0);

        let flags = if keep_cache {
            FopenFlags::FOPEN_KEEP_CACHE
        } else {
            FopenFlags::empty()
        };
        reply.opened(FileHandle(fh), flags);
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        self.block_on(async move {
            match self.read(fh.0, offset, size).await {
                Ok(data) => reply.data(&data),
                Err(e) => reply.error(map_mount_error(&e)),
            }
        });
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: fuser::WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyWrite,
    ) {
        match self.handles.write_at(fh.0, offset, data) {
            Ok(()) => {}
            Err(crate::handle::WriteAtError::InvalidHandle) => {
                reply.error(Errno::EBADF);
                return;
            }
            Err(crate::handle::WriteAtError::TooLarge) => {
                reply.error(Errno::EFBIG);
                return;
            }
        }
        reply.written(data.len() as u32);
    }

    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        self.block_on(async move {
            match self.flush(fh.0).await {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(map_mount_error(&e)),
            }
        });
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.block_on(async move {
            match self.release(fh.0).await {
                Ok(()) => reply.ok(),
                Err(e) => {
                    tracing::error!("release failed for fh={}: {e}", fh.0);
                    reply.error(map_mount_error(&e));
                }
            }
        });
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        self.block_on(async move {
            match self.flush(fh.0).await {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(map_mount_error(&e)),
            }
        });
    }

    fn opendir(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        reply.opened(FileHandle(0), FopenFlags::empty());
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let path = match self.inodes.get_path(ino.0) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let path_str = path.to_string_lossy().to_string();
        let inodes = self.inodes.clone();

        self.block_on(async move {
            match self.readdir(&path_str).await {
                Ok(entries) => {
                    let mut idx: u64 = 1;
                    if offset < idx {
                        let _ = reply.add(ino, idx, FileType::Directory, ".");
                    }
                    idx += 1;
                    if offset < idx {
                        let _ = reply.add(INodeNo(ROOT_INODE), idx, FileType::Directory, "..");
                    }
                    idx += 1;
                    for entry in &entries {
                        if offset < idx {
                            let child_path = path.join(&entry.name);
                            let kind = if entry.dir {
                                NodeKind::Dir
                            } else {
                                NodeKind::File
                            };
                            let child_ino = inodes.get_or_insert(&child_path, kind);
                            let ft = if entry.dir {
                                FileType::Directory
                            } else {
                                FileType::RegularFile
                            };
                            let _ = reply.add(INodeNo(child_ino), idx, ft, &entry.name);
                        }
                        idx += 1;
                    }
                    reply.ok();
                }
                Err(e) => reply.error(map_mount_error(&e)),
            }
        });
    }

    fn readdirplus(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectoryPlus,
    ) {
        let path = match self.inodes.get_path(ino.0) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let path_str = path.to_string_lossy().to_string();
        let inodes = self.inodes.clone();

        self.block_on(async move {
            match self.readdir(&path_str).await {
                Ok(entries) => {
                    let dir_attr = FileAttr {
                        ino,
                        size: 0,
                        blocks: 0,
                        atime: UNIX_EPOCH,
                        mtime: UNIX_EPOCH,
                        ctime: UNIX_EPOCH,
                        crtime: UNIX_EPOCH,
                        kind: FileType::Directory,
                        perm: 0o755,
                        nlink: 2,
                        uid: 0,
                        gid: 0,
                        rdev: 0,
                        blksize: 1 << 20,
                        flags: 0,
                    };
                    let mut idx: u64 = 1;
                    if offset < idx {
                        reply.add(ino, idx, ".", &TTL, &dir_attr, Generation(0));
                    }
                    idx += 1;
                    if offset < idx {
                        let root_attr = FileAttr {
                            ino: INodeNo(ROOT_INODE),
                            ..dir_attr
                        };
                        reply.add(
                            INodeNo(ROOT_INODE),
                            idx,
                            "..",
                            &TTL,
                            &root_attr,
                            Generation(0),
                        );
                    }
                    idx += 1;
                    for entry in &entries {
                        if offset < idx {
                            let child_path = path.join(&entry.name);
                            let kind = if entry.dir {
                                NodeKind::Dir
                            } else {
                                NodeKind::File
                            };
                            let child_ino = inodes.get_or_insert(&child_path, kind);
                            let attr = entry_to_file_attr(entry, child_ino);
                            reply.add(
                                INodeNo(child_ino),
                                idx,
                                &entry.name,
                                &TTL,
                                &attr,
                                Generation(0),
                            );
                        }
                        idx += 1;
                    }
                    reply.ok();
                }
                Err(e) => reply.error(map_mount_error(&e)),
            }
        });
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let parent_path = match self.inodes.get_path(parent.0) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let dir_path = parent_path.join(name);
        let dir_str = dir_path.to_string_lossy().to_string();
        let backend = self.backend.clone();
        let inodes = self.inodes.clone();
        let cache = self.cache.clone();

        self.block_on(async move {
            match backend.mkdir(&dir_str).await {
                Ok(()) => {
                    cache.invalidate_parent(&dir_str).await;
                    let ino = inodes.get_or_insert(&dir_path, NodeKind::Dir);
                    let attr = FileAttr {
                        ino: INodeNo(ino),
                        size: 0,
                        blocks: 0,
                        atime: UNIX_EPOCH,
                        mtime: UNIX_EPOCH,
                        ctime: UNIX_EPOCH,
                        crtime: UNIX_EPOCH,
                        kind: FileType::Directory,
                        perm: 0o755,
                        nlink: 2,
                        uid: 0,
                        gid: 0,
                        rdev: 0,
                        blksize: 1 << 20, // 1 MB — optimal for network FS I/O batching
                        flags: 0,
                    };
                    reply.entry(&TTL, &attr, Generation(0));
                }
                Err(e) => reply.error(map_backend_error(&e)),
            }
        });
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let parent_path = match self.inodes.get_path(parent.0) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let dir_path = parent_path.join(name);
        let dir_str = dir_path.to_string_lossy().to_string();
        let backend = self.backend.clone();
        let inodes = self.inodes.clone();
        let cache = self.cache.clone();

        self.block_on(async move {
            match backend.delete(&dir_str).await {
                Ok(()) => {
                    inodes.remove(&dir_path);
                    cache.invalidate(&dir_str).await;
                    cache.invalidate_parent(&dir_str).await;
                    reply.ok();
                }
                Err(e) => reply.error(map_backend_error(&e)),
            }
        });
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let parent_path = match self.inodes.get_path(parent.0) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let file_path = parent_path.join(name);
        let file_str = file_path.to_string_lossy().to_string();
        let backend = self.backend.clone();
        let inodes = self.inodes.clone();
        let cache = self.cache.clone();

        self.block_on(async move {
            match backend.delete(&file_str).await {
                Ok(()) => {
                    inodes.remove(&file_path);
                    cache.invalidate(&file_str).await;
                    cache.invalidate_parent(&file_str).await;
                    reply.ok();
                }
                Err(e) => reply.error(map_backend_error(&e)),
            }
        });
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        _flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let old_parent = match self.inodes.get_path(parent.0) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let new_parent = match self.inodes.get_path(newparent.0) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let old_path = old_parent.join(name);
        let new_path = new_parent.join(newname);
        let old_str = old_path.to_string_lossy().to_string();
        let new_str = new_path.to_string_lossy().to_string();
        let backend = self.backend.clone();
        let inodes = self.inodes.clone();
        let cache = self.cache.clone();

        self.block_on(async move {
            match backend.rename(&old_str, &new_str).await {
                Ok(()) => {
                    inodes.rename_subtree(&old_path, &new_path);
                    cache.invalidate(&old_str).await;
                    cache.invalidate(&new_str).await;
                    cache.invalidate_parent(&old_str).await;
                    cache.invalidate_parent(&new_str).await;
                    reply.ok();
                }
                Err(e) => reply.error(map_backend_error(&e)),
            }
        });
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let parent_path = match self.inodes.get_path(parent.0) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let file_path = parent_path.join(name);
        let file_str = file_path.to_string_lossy().to_string();
        let backend = self.backend.clone();
        let inodes = self.inodes.clone();
        let cache = self.cache.clone();

        let fh = self.handles.allocate(file_str.clone());
        self.block_on(async move {
            match backend.write(&file_str, &[]).await {
                Ok(()) => {
                    cache.invalidate_parent(&file_str).await;
                    let ino = inodes.get_or_insert(&file_path, NodeKind::File);
                    let attr = FileAttr {
                        ino: INodeNo(ino),
                        size: 0,
                        blocks: 0,
                        atime: UNIX_EPOCH,
                        mtime: UNIX_EPOCH,
                        ctime: UNIX_EPOCH,
                        crtime: UNIX_EPOCH,
                        kind: FileType::RegularFile,
                        perm: 0o644,
                        nlink: 1,
                        uid: 0,
                        gid: 0,
                        rdev: 0,
                        blksize: 1 << 20, // 1 MB — optimal for network FS I/O batching
                        flags: 0,
                    };
                    reply.created(
                        &TTL,
                        &attr,
                        Generation(0),
                        FileHandle(fh),
                        FopenFlags::empty(),
                    );
                }
                Err(e) => {
                    self.handles.remove(fh);
                    reply.error(map_backend_error(&e));
                }
            }
        });
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        reply.statfs(0, 0, 0, 0, 0, 4096, 255, 4096);
    }
}

// ---------------------------------------------------------------------------
// Mount helper (Linux-specific)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub fn mount_linux<B: StorageBackend>(
    adapter: FuseAdapter<B>,
    config: &crate::mount::MountConfig,
    status: &std::sync::Mutex<crate::mount::MountStatus>,
    event_tx: &tokio::sync::broadcast::Sender<crate::mount::MountEvent>,
) -> Result<(), MountError> {
    let mut options = vec![
        MountOption::FSName("rs-f4ss".to_string()),
        MountOption::DefaultPermissions,
    ];
    if config.read_only {
        options.push(MountOption::RO);
    }
    let acl = if config.allow_other {
        SessionACL::All
    } else {
        SessionACL::Owner
    };

    *status.lock().unwrap() = crate::mount::MountStatus::Mounted;
    if let Some(ref cb) = config.on_mount_ready {
        cb();
    }
    let mountpoint = config.mountpoint.clone();
    let mut fuse_config = Config::default();
    fuse_config.mount_options = options;
    fuse_config.acl = acl;
    let result = fuser::mount(adapter, &mountpoint, &fuse_config);

    event_tx.send(crate::mount::MountEvent::MountStopped).ok();
    *status.lock().unwrap() = crate::mount::MountStatus::Idle;
    result.map_err(|e| MountError::FuseError(e.to_string()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn test_entry_to_file_attr_file() {
        let entry = Entry {
            path: "/file.txt".to_string(),
            name: "file.txt".to_string(),
            dir: false,
            size: 1024,
            mtime: SystemTime::UNIX_EPOCH,
        };
        let attr = entry_to_file_attr(&entry, 42);
        assert_eq!(attr.ino, INodeNo(42));
        assert_eq!(attr.size, 1024);
        assert_eq!(attr.kind, FileType::RegularFile);
        assert_eq!(attr.perm, 0o644);
    }

    #[test]
    fn test_entry_to_file_attr_dir() {
        let entry = Entry {
            path: "/dir".to_string(),
            name: "dir".to_string(),
            dir: true,
            size: 0,
            mtime: SystemTime::UNIX_EPOCH,
        };
        let attr = entry_to_file_attr(&entry, 7);
        assert_eq!(attr.kind, FileType::Directory);
        assert_eq!(attr.perm, 0o755);
        assert_eq!(attr.nlink, 2);
    }

    #[test]
    fn test_map_backend_error_not_found() {
        assert_eq!(
            map_backend_error(&BackendError::NotFound("test".into())),
            Errno::ENOENT
        );
    }

    #[test]
    fn test_map_backend_error_permission() {
        assert_eq!(
            map_backend_error(&BackendError::PermissionDenied("test".into())),
            Errno::EACCES
        );
    }

    #[test]
    fn test_map_backend_error_readonly() {
        assert_eq!(map_backend_error(&BackendError::ReadOnly), Errno::EROFS);
    }
}
