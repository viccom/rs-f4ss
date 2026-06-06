# rs-f4ss Specification

**Version**: 1.2.0
**Status**: Phase 1 + Phase 2 Complete (see TASKS.md for details)
**Date**: 2026-06-05

> **Note**: This SPEC covers Phase 1 architecture and API. Phase 2 additions (HTTP backend,
> REST API, MountManager, embedded Web UI, Tauri desktop, daemon mode, performance optimizations)
> are all implemented. See `CLAUDE.md` for current architecture and `TASKS.md` for completion status.
> Some sections below (crate structure, test counts, CLI examples) are outdated — refer to `CLAUDE.md`
> for current values.

**Changelog**:
- v1.1.0: WinFsp support, code review fixes (P0-P3), TDD audit, Phase 2 planning
- v1.0.0: Initial Phase 1 implementation

---

## 1. Overview

`rs-f4ss` is a tool suite for mounting remote file servers as local filesystems via FUSE. It provides a **core library** (`rs-f4ss-core`) with pluggable storage backends, cache, and mount engine, plus **frontend adapters** for CLI, Web, and Tauri desktop.

**Phase 1** implements WebDAV only. The architecture supports future backends (SFTP, S3, HTTP listing, etc.) via the `StorageBackend` trait.

```
┌─────────────────────────────────────────────────────────────┐
│                      Frontend Adapters                       │
│  ┌──────────┐   ┌──────────┐   ┌──────────────────────────┐ │
│  │ CLI      │   │ Web      │   │ Tauri Desktop            │ │
│  │ (clap)   │   │ (axum)   │   │ (tauri + system tray)    │ │
│  └────┬─────┘   └────┬─────┘   └────────────┬─────────────┘ │
│       │              │                      │               │
│  ┌────┴──────────────┴──────────────────────┴─────────────┐ │
│  │              rs-f4ss-core (library)                  │ │
│  │  ┌──────────┐ ┌──────────┐ ┌─────────────────────────┐ │ │
│  │  │ Mount    │ │ Cache    │ │ StorageBackend (trait)   │ │ │
│  │  │ Engine   │ │ Layer    │ │ ┌───────┐ ┌───────────┐ │ │ │
│  │  │          │ │          │ │ │ WebDAV│ │ SFTP/S3/..│ │ │ │
│  │  │          │ │          │ │ │ (impl)│ │ (future)  │ │ │ │
│  │  └──────────┘ └──────────┘ │ └───────┘ └───────────┘ │ │ │
│  │                            └─────────────────────────┘ │ │
│  └────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
         │ FUSE │           │ HTTP/SFTP/S3/.. │
         ▼      ▼           ▼                 ▼
   ┌──────────┐        ┌─────────────────────────┐
   │ Local OS │        │  Remote Storage         │
   │ FS       │        │  (dufs/SFTP/S3/FTP/..)  │
   └──────────┘        └─────────────────────────┘
```

## 2. Design Principles

### 2.1 Separation of Concerns
- **Core library** (`rs-f4ss-core`): All business logic, no UI dependencies
- **Frontend adapters**: Thin wrappers that call core APIs
- **Zero coupling**: Core doesn't know about frontends; frontends don't know about each other

### 2.2 Pluggable Storage Backends
- **`StorageBackend` trait**: Abstract interface for all remote file operations
- **Phase 1**: WebDAV implementation only
- **Future**: SFTP, S3, HTTP listing, FTP — each is a separate backend implementing the trait
- **MountEngine**: Uses `Box<dyn StorageBackend>` for runtime dispatch; `MountEngineGeneric<B>` available for compile-time static dispatch

### 2.3 Core Library as Single Source of Truth
All frontends share the same:
- Storage backends (connection pooling, auth, retry)
- Cache layer (metadata LRU, directory LRU)
- Mount engine (FUSE implementation)
- Error handling and logging

### 2.4 Event-Driven Architecture
Core emits events; frontends subscribe and react:
```rust
enum MountEvent {
    Connected { url: String },
    MountStarted { mountpoint: PathBuf },
    FileRead { path: PathBuf, bytes: u64 },
    FileWritten { path: PathBuf, bytes: u64 },
    Error { error: MountError },
    CacheHit { path: PathBuf },
    CacheMiss { path: PathBuf },
}
```

### 2.5 Async-First (with FUSE sync bridge)
All backend APIs are async (`tokio`). The FUSE layer uses a sync-to-async bridge:
- **Backend** (`StorageBackend`): Fully async via `tokio`
- **FUSE** (`FuseAdapter`): Synchronous callbacks (required by `fuser`). Uses `block_on()` to bridge to async backend calls.
- **Frontends**: CLI uses `std::thread` for mount (blocking); Web/Tauri will use their own async runtimes.

## 3. Crate Structure

```
rs-f4ss/
├── Cargo.toml              # Workspace root
├── crates/
│   ├── rs-f4ss-core/    # Core library (published crate)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── mount.rs        # FuseAdapter + MountEngine: FUSE + orchestration
│   │       ├── inode.rs        # Inode ↔ path bidirectional map (FNV-1a)
│   │       ├── backend/
│   │       │   ├── mod.rs      # StorageBackend trait definition
│   │       │   ├── webdav.rs   # WebDAV implementation (Phase 1)
│   │       │   ├── types.rs    # Backend-agnostic types (Entry)
│   │       │   ├── sftp.rs     # SFTP implementation (future)
│   │       │   ├── s3.rs       # S3 implementation (future)
│   │       │   └── http.rs     # HTTP listing (future)
│   │       ├── cache.rs        # Metadata cache (moka, attrs + children)
│   │       ├── handle.rs       # File handle table (write buffering)
│   │       └── error.rs        # Layered error types (BackendError + MountError)
│   │
│   ├── rs-f4ss-cli/     # CLI frontend
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs
│   │
│   ├── rs-f4ss-web/     # Web frontend (Phase 2)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs
│   │
│   └── rs-f4ss-app/     # Tauri desktop (Phase 3)
│       ├── Cargo.toml
│       ├── src/
│       │   └── main.rs
│       └── tauri.conf.json
│
├── docs/
│   └── SPEC.md
└── tests/                  # Integration tests
    ├── fixtures.rs
    └── ...
```

## 4. Core Library API (`rs-f4ss-core`)

### 4.1 MountEngine — Central Orchestrator

```rust
/// Generic mount engine. CLI uses Box<dyn StorageBackend> for runtime dispatch.
/// The cache, handle table, and inode map live inside FuseAdapter,
/// created when mount() is called.
pub struct MountEngine<B: StorageBackend> {
    backend: Option<B>,      // Option: consumed by mount()
    config: MountConfig,
    status: Mutex<MountStatus>,
    event_tx: broadcast::Sender<MountEvent>,
}

pub struct MountConfig {
    pub mountpoint: PathBuf,
    pub read_only: bool,
    pub cache_ttl: Duration,
    pub cache_size: usize,
    pub allow_other: bool,
}

pub enum MountStatus {
    Idle, Mounting, Mounted, Unmounting, Error(String),
}

impl<B: StorageBackend> MountEngine<B> {
    pub fn new(backend: B, config: MountConfig) -> Self;
    pub fn status(&self) -> MountStatus;
    pub fn mountpoint(&self) -> &Path;
    pub fn subscribe(&self) -> broadcast::Receiver<MountEvent>;

    /// Start the FUSE mount. Synchronous — blocks until unmount.
    /// Creates FuseAdapter internally (owns cache, handles, inodes).
    /// Uses fuser::mount() with Config struct and SessionACL.
    pub fn mount(self) -> Result<(), MountError>;
}
```

**Design note**: `mount()` is synchronous because `fuser::mount()` blocks the calling thread.
The async backend calls are bridged via `block_on()` inside `FuseAdapter`.
Unmount is handled externally via `fusermount -u` (future: CLI `unmount` subcommand).
```

### 4.2 StorageBackend Trait — Pluggable Protocol Interface

```rust
/// Abstract interface for remote file storage operations.
/// Implement this trait to add support for new protocols (SFTP, S3, etc.)
#[async_trait]
pub trait StorageBackend: Send + Sync + 'static {
    /// Protocol name for display (e.g., "webdav", "sftp", "s3")
    fn protocol(&self) -> &str;

    /// Server address for display (e.g., "http://192.168.1.100:5000")
    fn server_addr(&self) -> &str;

    /// Whether this backend is read-only
    fn is_read_only(&self) -> bool { false }

    /// List directory contents
    async fn list(&self, path: &str) -> Result<Vec<Entry>, BackendError>;

    /// Get file/directory attributes
    async fn stat(&self, path: &str) -> Result<Entry, BackendError>;

    /// Read file content with range (offset, size).
    /// size is u32 to match FUSE read callback signature.
    async fn read(&self, path: &str, offset: u64, size: u32) -> Result<Vec<u8>, BackendError>;

    /// Write file content (full upload)
    async fn write(&self, path: &str, data: &[u8]) -> Result<(), BackendError>;

    /// Create directory
    async fn mkdir(&self, path: &str) -> Result<(), BackendError>;

    /// Delete file or directory
    async fn delete(&self, path: &str) -> Result<(), BackendError>;

    /// Rename/move file or directory
    async fn rename(&self, from: &str, to: &str) -> Result<(), BackendError>;

    /// Health check (optional, default: always Ok)
    async fn ping(&self) -> Result<bool, BackendError> { Ok(true) }
}

/// Backend-agnostic entry type
pub struct Entry {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Modification time. Defaults to `SystemTime::UNIX_EPOCH` if server
    /// does not provide `getlastmodified` (e.g., some minimal WebDAV servers).
    pub mtime: SystemTime,
}

/// Backend-specific errors
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    #[error("Protocol error: {0}")]
    ProtocolError(String),
    #[error("Read-only backend")]
    ReadOnly,
    #[error("Invalid path: {0}")]
    InvalidPath(String),       // path traversal, null bytes
    #[error("Operation not supported: {0}")]
    NotSupported(String),      // 413 payload too large, etc.
    #[error("Internal error: {0}")]
    Internal(String),          // unexpected HTTP status, client errors
}
```

### 4.3 WebDavBackend — WebDAV Implementation (Phase 1)

```rust
pub struct WebDavBackend {
    client: reqwest::Client,
    base_url: Url,                // parsed url::Url
    auth_header: Option<String>,  // "Basic <base64>"
    read_only: bool,
}

impl WebDavBackend {
    pub fn new(base_url: &str, read_only: bool) -> Result<Self, BackendError>;
    pub fn new_with_auth(base_url: &str, read_only: bool,
                         username: Option<&str>, password: Option<&str>) -> Result<Self, BackendError>;
    pub fn build_url(&self, path: &str) -> Result<String, BackendError>;
}

#[async_trait]
impl StorageBackend for WebDavBackend {
    fn protocol(&self) -> &str { "webdav" }
    fn server_addr(&self) -> &str { self.base_url.as_str() }
    fn is_read_only(&self) -> bool { self.read_only }

    async fn list(&self, path: &str) -> Result<Vec<Entry>, BackendError> {
        // PROPFIND depth=1, parse XML response, filter self-entry
    }

    async fn stat(&self, path: &str) -> Result<Entry, BackendError> {
        // PROPFIND depth=0, parse XML response
    }

    async fn read(&self, path: &str, offset: u64, size: u32) -> Result<Vec<u8>, BackendError> {
        // GET whole file, slice to [offset..offset+size].
        // Avoids Range header to prevent 416 errors on small files.
    }

    async fn write(&self, path: &str, data: &[u8]) -> Result<(), BackendError> {
        // PUT entire content
    }

    async fn mkdir(&self, path: &str) -> Result<(), BackendError> {
        // MKCOL
    }

    async fn delete(&self, path: &str) -> Result<(), BackendError> {
        // DELETE
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), BackendError> {
        // MOVE with Destination + Overwrite: T headers
    }

    async fn ping(&self) -> Result<bool, BackendError> {
        // Default: Ok(true). Future: GET /__dufs__/health
    }
}
```

### 4.4 CacheLayer — Metadata Cache

```rust
#[derive(Clone)]
pub struct CacheLayer {
    attrs: Cache<String, CachedAttr>,       // path → metadata
    children: Cache<String, CachedChildren>, // path → directory listing
}

pub struct CachedAttr {
    pub entry: Entry,
}

pub struct CachedChildren {
    pub entries: Vec<Entry>,
}

impl CacheLayer {
    pub fn new(ttl: Duration, max_size: u64) -> Self;

    pub async fn get_attr(&self, path: &str) -> Option<CachedAttr>;
    pub async fn set_attr(&self, path: &str, attr: CachedAttr);
    pub async fn get_children(&self, path: &str) -> Option<CachedChildren>;
    pub async fn set_children(&self, path: &str, children: CachedChildren);
    pub async fn invalidate(&self, path: &str);
    pub async fn invalidate_parent(&self, path: &str);  // normalizes "" → "/"
    pub async fn clear(&self);
}
```

### 4.5 Event System

```rust
#[derive(Clone, Debug, Serialize)]
pub enum MountEvent {
    Connected { url: String },
    MountStarted { mountpoint: PathBuf },
    MountStopped,
    FileRead { path: PathBuf, bytes: u64, duration_ms: u64 },
    FileWritten { path: PathBuf, bytes: u64, duration_ms: u64 },
    DirListed { path: PathBuf, entries: usize },
    CacheHit { path: PathBuf },
    CacheMiss { path: PathBuf },
    Error { error: String },         // String (not MountError) for Serialize
}
```

### 4.6 Error Types (Layered Model)

Errors are split into two layers: backend protocol errors and FUSE mount errors.

```rust
/// Layer 1: Backend protocol errors (HTTP/WebDAV specific)
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    #[error("Protocol error: {0}")]
    ProtocolError(String),
    #[error("Read-only backend")]
    ReadOnly,
    #[error("Invalid path: {0}")]
    InvalidPath(String),
    #[error("Operation not supported: {0}")]
    NotSupported(String),
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Layer 2: FUSE mount errors (wraps BackendError)
#[derive(Debug, thiserror::Error)]
pub enum MountError {
    #[error("FUSE error: {0}")]
    FuseError(String),
    #[error("Authentication failed")]
    AuthFailed,
    #[error("Backend error: {0}")]
    Backend(#[from] BackendError),
    #[error("Mount point error: {0}")]
    MountPoint(String),
    #[error("Configuration error: {0}")]
    Config(String),
}

/// Map BackendError → fuser::Errno for FUSE replies.
pub fn map_backend_error(err: &BackendError) -> fuser::Errno;
```

## 5. Frontend Adapters

### 5.1 CLI (`rs-f4ss-cli`) — Phase 1

```rust
// main.rs — synchronous main (mount() blocks)
fn main() {
    let cli = Cli::parse();   // clap derive
    let backend = resolve_backend(&cli.url, cli.read_only, cli.user.as_deref(), cli.pass.as_deref())?;
    let config = MountConfig { mountpoint, read_only, cache_ttl, cache_size, allow_other };
    let engine = MountEngine::new(backend, config);

    // Subscribe to events for logging (separate thread)
    let mut events = engine.subscribe();
    std::thread::spawn(move || {
        while let Ok(event) = events.blocking_recv() { /* tracing log */ }
    });

    engine.mount()?;  // synchronous — blocks until fusermount -u
}

fn resolve_backend(url, read_only, username, password) -> Result<Box<dyn StorageBackend>> {
    // http/https/webdav/webdavs → WebDavBackend::new_with_auth()
    // sftp/s3/ftp → Err("Unsupported protocol")
    // no scheme → Err("Invalid URL")
}
```

```bash
# CLI usage (Phase 1 — WebDAV)
rs-f4ss http://192.168.1.100:5000 /mnt/dufs --user admin --pass secret
rs-f4ss webdav://192.168.1.100:5000 /mnt/dufs --read-only --foreground
rs-f4ss http://host:5000 /mnt --cache-ttl 30 --cache-size 512

# Unmount (external, until CLI unmount subcommand is implemented)
fusermount -u /mnt/dufs

# Future: CLI subcommands
# rs-f4ss status                  # Show current mounts
# rs-f4ss unmount /mnt/dufs       # Unmount
```

### 5.2 Web (`rs-f4ss-web`) — Phase 2

```rust
// main.rs — axum web server
#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/api/mount", post(mount_handler))
        .route("/api/mount/:id", delete(unmount_handler))
        .route("/api/mounts", get(list_mounts_handler))
        .route("/api/events", get(websocket_events_handler))
        .route("/", get(index_handler));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:37000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

```
Web UI:
┌─────────────────────────────────────────────────┐
│  rs-f4ss Web Manager                         │
│                                                 │
│  ┌─────────────────────────────────────────────┐│
│  │ Mount URL: [http://192.168.1.100:5000     ]││
│  │ Mountpoint: [/mnt/dufs                     ]││
│  │ User: [admin    ]  Pass: [••••    ]         ││
│  │ ☐ Read-only  ☐ Allow other                 ││
│  │ [Mount]                                     ││
│  └─────────────────────────────────────────────┘│
│                                                 │
│  Active Mounts:                                 │
│  ┌─────────────────────────────────────────────┐│
│  │ /mnt/dufs  ← http://192.168.1.100:5000     ││
│  │ Status: Connected  │  Cache: 42 hits        ││
│  │ [Unmount] [Stats]                          ││
│  └─────────────────────────────────────────────┘│
│                                                 │
│  Event Log:                                     │
│  10:00:01 Connected to http://192.168.1.100:5000│
│  10:00:02 Mount started at /mnt/dufs            │
│  10:00:05 READ /docs/file.txt (1.2KB, 3ms)     │
│  10:00:08 WRITE /docs/new.txt (512B, 12ms)     │
└─────────────────────────────────────────────────┘
```

### 5.3 Tauri Desktop (`rs-f4ss-app`) — Phase 3

```rust
// main.rs
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // System tray
            let tray = app.tray_by_id("main").unwrap();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            mount, unmount, list_mounts, get_status, get_events
        ])
        .run(tauri::generate_context!())
        .expect("error running tauri app");
}

#[tauri::command]
async fn mount(url: String, mountpoint: String, config: MountConfig) -> Result<(), String> {
    let engine = MountEngine::new(config);
    engine.mount().await.map_err(|e| e.to_string())
}
```

```
Tauri Desktop:
┌─────────────────────────────────────────────────┐
│  rs-f4ss                          ─ □ ✕      │
│─────────────────────────────────────────────────│
│                                                 │
│  Quick Mount                                    │
│  ┌─────────────────────────────────────────────┐│
│  │ Server: [http://192.168.1.100:5000        ]││
│  │ [Browse...]  [Mount]                        ││
│  └─────────────────────────────────────────────┘│
│                                                 │
│  Active Mounts                                  │
│  ┌─────────────────────────────────────────────┐│
│  │ 📁 /mnt/dufs                                ││
│  │    ← http://192.168.1.100:5000              ││
│  │    Status: Connected │ Reads: 128 │ Writes: 5││
│  │    [Open Folder] [Unmount] [Settings]       ││
│  └─────────────────────────────────────────────┘│
│                                                 │
│  Recent Activity                                │
│  10:00:05 READ /docs/file.txt (1.2KB)          │
│  10:00:08 WRITE /docs/new.txt (512B)           │
│  10:00:12 CACHE HIT /docs/file.txt             │
│                                                 │
│─────────────────────────────────────────────────│
│  Settings │ Logs │ About              v0.1.0    │
└─────────────────────────────────────────────────┘

System Tray:
┌───────────────┐
│ 📁 rs-f4ss │
│───────────────│
│ Mount...      │
│ Unmount All   │
│───────────────│
│ 1 mount active│
│ Open Manager  │
│───────────────│
│ Quit          │
└───────────────┘
```

## 6. fuser::Filesystem Implementation

The FUSE layer is **backend-agnostic** — it calls `StorageBackend` trait methods, not WebDAV directly.
All callbacks are synchronous (required by fuser) and use `block_on()` to bridge to async backend calls.

```rust
impl<B: StorageBackend> fuser::Filesystem for FuseAdapter<B> {
    // All 20 methods are synchronous.
    // Internal structure:
    //   self.backend: Arc<B>         — shared backend
    //   self.cache: CacheLayer        — moka metadata cache
    //   self.handles: HandleTable     — file handle → OpenFile
    //   self.inodes: Arc<InodeMap>    — inode ↔ path bidirectional map
    //   self.rt: tokio::Runtime      — async bridge

    fn getattr(&self, _req, ino, fh, reply: ReplyAttr) {
        let path = self.inodes.get_path(ino.0);  // inode → path
        self.block_on(async {
            let entry = self.backend.stat(&path_str).await;
            reply.attr(&TTL, &entry_to_file_attr(&entry, ino));
        });
    }
    // ... 15 more methods
}
```

### 6.1 Implemented FUSE Methods (20 total)

> **Note**: The `FuseAdapter<B>` struct owns the `CacheLayer`, `HandleTable`,
> and `InodeMap` internally. All callbacks use `block_on()` to bridge to async backend.

#### `init(&mut self, req, config) -> io::Result<()>`
- No-op. Logs FUSE init.

#### `destroy(&mut self)`
- No-op. Logs FUSE destroy.

#### `lookup(parent, name) -> ReplyEntry`
- **Path**: `inodes.get_path(parent)` + join name → `stat(path_str)`
- **Inode**: Registers new inode via `inodes.get_or_insert()`
- **Reply**: `reply.entry(ttl, attr, Generation(0))`

#### `forget(req, ino, nlookup)`
- Removes inode mapping via `inodes.remove_by_inode(ino)`

#### `getattr(ino, fh) -> ReplyAttr`
- **Path**: `inodes.get_path(ino)` → `backend.stat(path_str)`
- **Cache**: No cache in FUSE layer (cache is in inherent methods only)
- **Reply**: `reply.attr(ttl, attr)`

#### `setattr(ino, mode, uid, gid, size, ..., flags) -> ReplyAttr`
- **Truncate**: If `size == Some(0)`, calls `backend.write(path, &[])`
- **Others**: No-op (mode/uid/gid/timestamps not supported by WebDAV)
- **Reply**: Re-stats the file after truncate

#### `open(ino, flags) -> ReplyOpen`
- Allocates handle via `handles.allocate(path)`
- **Reply**: `reply.opened(fh, FopenFlags::empty())`

#### `read(ino, fh, offset, size, flags, lock_owner) -> ReplyData`
- Resolves path from handle table
- **Backend**: `backend.read(path, offset, size)` (full GET + slice)
- **Reply**: `reply.data(&data)`

#### `write(ino, fh, offset, data, write_flags, flags, lock_owner) -> ReplyWrite`
- Buffers data via `handles.write_at(fh, offset, data)`
- **Reply**: `reply.written(data.len())` (no I/O yet)

#### `flush(ino, fh, lock_owner) -> ReplyEmpty`
- Takes dirty buffer via `handles.take_dirty(fh)`
- **Backend**: `backend.write(path, &buffer)` if dirty
- **Cache**: Invalidates path + parent after write

#### `release(ino, fh, flags, flush, lock_owner) -> ReplyEmpty`
- Removes handle from table
- **Fallback**: If still dirty (flush wasn't called), uploads buffer

#### `opendir(ino, flags) -> ReplyOpen`
- No-op. Returns handle 0.

#### `readdir(ino, fh, offset) -> ReplyDirectory`
- **Backend**: `backend.list(path_str)`
- Adds `.` and `..` entries, then directory children
- Registers child inodes via `inodes.get_or_insert()`

#### `readdirplus(ino, fh, offset) -> ReplyDirectoryPlus`
- Same as readdir but includes `FileAttr` for each entry
- Uses `list()` Entry data directly for child attrs + 1 extra `stat()` for parent directory

#### `mkdir(parent, name, mode, umask) -> ReplyEntry`
- **Backend**: `backend.mkdir(path_str)`
- **Cache**: Invalidates parent directory listing

#### `rmdir(parent, name) -> ReplyEmpty`
- **Backend**: `backend.delete(path_str)`
- **Cache**: Invalidates path + parent
- **Note**: No ENOTEMPTY check (delegated to backend; dufs supports recursive delete)

#### `unlink(parent, name) -> ReplyEmpty`
- **Backend**: `backend.delete(path_str)`
- **Cache**: Invalidates path + parent

#### `rename(parent, name, newparent, newname, flags) -> ReplyEmpty`
- **Backend**: `backend.rename(old_str, new_str)`
- **Inode**: `inodes.rename_subtree(old_path, new_path)`
- **Cache**: Invalidates old path + both parent directories

#### `create(parent, name, mode, umask, flags) -> ReplyCreate`
- **Backend**: `backend.write(path_str, &[])` (creates empty file)
- Allocates file handle immediately
- **Reply**: `reply.created(ttl, attr, Generation(0), fh, flags)`

#### `statfs(ino) -> ReplyStatfs`
- Returns synthetic values: all zero (unknown capacity)
- **Reply**: `reply.statfs(0, 0, 0, 0, 0, 4096, 255, 4096)`

### 6.2 Unsupported Methods (Phase 1)

| Method | Reason |
|--------|--------|
| `symlink` | Not supported by WebDAV or most backends |
| `readlink` | Not supported by WebDAV or most backends |
| `setxattr` | Not supported by WebDAV or most backends |
| `getxattr` | Not supported by WebDAV (fuser logs "Not Implemented") |
| `listxattr` | Not supported by WebDAV or most backends |
| `removexattr` | Not supported by WebDAV or most backends |

## 7. Cache Strategy

### 7.1 Metadata Cache (Two Separate Caches)
```
┌──────────────────────────────┐  ┌──────────────────────────────┐
│      Attributes Cache        │  │     Children Cache           │
│  key: String (path)          │  │  key: String (path)          │
│  value: CachedAttr {         │  │  value: CachedChildren {     │
│    entry: Entry,             │  │    entries: Vec<Entry>,      │
│  }                           │  │  }                           │
│  ttl: 5s (via --cache-ttl)  │  │  ttl: 5s (via --cache-ttl)  │
│  max: 256 (via --cache-size)│  │  max: 256 (via --cache-size)│
└──────────────────────────────┘  └──────────────────────────────┘
Both backed by moka::future::Cache. Clonable for closure captures.
```

### 7.2 Cache Invalidation
- **TTL-based**: Entries expire after `cache-ttl` seconds
- **Write-through**: Writes invalidate the path's cache entry
- **Directory invalidation**: `mkdir`, `rmdir`, `unlink`, `rename` invalidate parent directory cache
- **No negative caching**: Don't cache "not found" results

### 7.3 File Handle Table
```
┌─────────────────────────────────────┐
│          File Handle Table          │
│  next_fh: AtomicU64 (incrementing)  │
│  files: Mutex<HashMap<u64, OpenFile>│
│                                     │
│  OpenFile {                         │
│    path: String,                    │
│    dirty: bool,                     │
│    buffer: Vec<u8>,  // grows on    │
│                      // write_at()  │
│  }                                  │
│                                     │
│  Key methods:                       │
│  - allocate(path) → u64             │
│  - get_path(fh) → Option<String>   │
│  - write_at(fh, off, data) → bool  │
│  - take_dirty(fh) → Option<(..)>   │
│  - remove(fh) → Option<OpenFile>   │
└─────────────────────────────────────┘
```

## 8. Protocol Mapping

### 8.1 StorageBackend → FUSE Mapping

| FUSE Operation | Backend Method | Notes |
|---------------|---------------|-------|
| `getattr` | `backend.stat(path)` | Single resource attributes |
| `lookup` | `backend.list(parent)` | Find entry by name |
| `readdir` | `backend.list(path)` | Directory listing |
| `read` | `backend.read(path, offset, size)` | Range read |
| `write` | `backend.write(path, data)` | Full file upload |
| `mkdir` | `backend.mkdir(path)` | Create directory |
| `rmdir` | `backend.delete(path)` | Remove directory |
| `unlink` | `backend.delete(path)` | Remove file |
| `rename` | `backend.rename(from, to)` | Move/rename |

### 8.2 WebDAV-Specific Implementation

| FUSE Operation | Backend Method | WebDAV Method | Headers |
|---------------|---------------|---------------|---------|
| `getattr` | `stat` | `PROPFIND` | `Depth: 0` |
| `lookup` | `list` | `PROPFIND` | `Depth: 1` on parent |
| `readdir` | `list` | `PROPFIND` | `Depth: 1` |
| `read` | `read` | `GET` | Full file fetch, slice in-memory |
| `write` | `write` | `PUT` | Content-Length, Content-Type |
| `mkdir` | `mkdir` | `MKCOL` | |
| `rmdir` | `delete` | `DELETE` | |
| `unlink` | `delete` | `DELETE` | |
| `rename` | `rename` | `MOVE` | `Destination: url`, `Overwrite: T` |

### 8.3 Response Parsing

WebDAV XML response for `PROPFIND`:
```xml
<D:multistatus>
  <D:response>
    <D:href>/path/to/file.txt</D:href>
    <D:propstat>
      <D:prop>
        <D:getcontentlength>1234</D:getcontentlength>
        <D:getlastmodified>Fri, 30 May 2026 10:00:00 GMT</D:getlastmodified>
        <D:resourcetype/>  <!-- empty = file -->
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>
```

Mapping to `FileAttr`:
- `getcontentlength` → `size: u64`
- `getlastmodified` → `mtime: SystemTime` (defaults to `UNIX_EPOCH` if absent)
- `resourcetype` has `<D:collection/>` → directory, else file
- Synthetic `mode`: 0o755 (dir) or 0o644 (file)

## 9. Error Handling

### 9.1 HTTP Status → BackendError → Errno Mapping

| HTTP Status | BackendError | Errno | Description |
|-------------|-------------|-------|-------------|
| 200, 201, 204 | (success) | OK | Success |
| 401 | PermissionDenied | EACCES | Auth required |
| 403 | PermissionDenied | EACCES | Forbidden |
| 404 | NotFound | ENOENT | Not found |
| 405 | InvalidPath | EINVAL | Method not allowed (dir exists) |
| 413 | NotSupported | ENOTSUP | File too large |
| 500, 502, 503, 504 | (retry) | EIO | Transient (retried) |
| Other non-success | Internal | EIO | Unexpected status |

### 9.2 Retry Strategy (`send_with_retry`)
- **Transient errors** (500, 502, 503, 504): Retry up to 3 times with exponential backoff (200ms, 400ms, 800ms)
- **Network errors** (connect/timeout): Retry up to 3 times with linear backoff (100ms, 200ms, 300ms)
- **Auth errors** (401): Fail immediately; don't retry

## 10. Dependencies

### 10.1 Core Library (`rs-f4ss-core`)

```toml
[dependencies]
fuser = { git = "https://github.com/cberner/fuser", branch = "master" }
async-trait = "0.1"
reqwest = { version = "0.12", features = ["rustls-tls"], default-features = false }
quick-xml = "0.36"
moka = { version = "0.12", features = ["future"] }
tokio = { version = "1", features = ["full"] }
dashmap = "6"              # Concurrent InodeMap
url = "2"
chrono = "0.4"
tracing = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"
base64 = "0.22"            # HTTP Basic Auth encoding
```

### 10.2 CLI (`rs-f4ss-cli`)

```toml
[dependencies]
rs-f4ss-core = { path = "../rs-f4ss-core" }
clap = { version = "4", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

### 10.3 Web (`rs-f4ss-web`) — Phase 2

```toml
[dependencies]
rs-f4ss-core = { path = "../rs-f4ss-core" }
axum = "0.7"
tokio = { version = "1", features = ["full"] }
tower-http = { version = "0.5", features = ["cors"] }
serde_json = "1"
```

### 10.4 Tauri (`rs-f4ss-app`) — Phase 3

```toml
[dependencies]
rs-f4ss-core = { path = "../rs-f4ss-core" }
tauri = { version = "2", features = ["tray-icon"] }
tokio = { version = "1", features = ["full"] }
serde_json = "1"
```

## 11. Testing Strategy

### 11.1 Unit Tests (per module) — 59 tests
- `error.rs`: Display impl (5) + errno mapping (9) = 14 tests
- `handle.rs`: Handle allocation, write buffer, dirty flag, offset writes = 7 tests
- `inode.rs`: FNV-1a hash, collision, get_or_insert, remove, rename_subtree = 7 tests
- `cache.rs`: Set/get attr, miss, invalidate, invalidate_parent, clear, children = 6 tests
- `backend/types.rs`: Entry dir/file, default mtime, path_name = 4 tests
- `backend/webdav.rs`: URL construction (10) + PROPFIND XML parsing (11) + backend constructor (4) = 25 tests
- (mount.rs tests reserved for integration)

### 11.2 Integration Tests — 5 tests
- `tests/integration.rs`: Full lifecycle against MockBackend
  - Read lifecycle (stat → open → read → release)
  - Write lifecycle (open → write → flush → stat)
  - Mkdir lifecycle
  - Delete lifecycle (create → unlink → stat NotFound)
  - Rename lifecycle (create a → rename to b → verify)

### 11.3 CLI Tests — 12 tests
- Argument parsing: basic, auth, read-only, cache-ttl, missing-url
- Backend resolution: http, https, webdav, webdavs, unsupported, no-scheme, auth-partial

### 11.4 E2E Tests — 36 tests (shell script)
- **Phase 1 — Normal mode** (30 tests):
  Read: readdir, stat file/dir, nested, binary integrity, readdirplus, recursive find, ENOENT, empty file
  Write: create, overwrite, 1MB large file, cache coherency, touch, cp compatibility
  Dir: mkdir, existing dir (EEXIST), rmdir non-empty, listing update
  Delete: unlink, ENOENT, rmdir empty
  Rename: same dir, cross-dir, directory with contents
  Advanced: statfs (df), burst writes, unicode, multi-read
- **Phase 2 — Read-only mode** (3 tests):
  Read succeeds, write blocked (EROFS), mkdir blocked (EROFS)
- **Phase 3 — Auth mode** (3 tests):
  Auth read/write/list with HTTP Basic credentials

## 12. Milestones

| Phase | Target | Scope | Code Lines | Status |
|-------|--------|-------|------------|--------|
| **Phase 1** | Week 1-2 | Core library + CLI + WebDAV backend | 2218 | ✅ Complete |
| **Phase 2** | Week 3-4 | Web frontend + API | ~800 | ⬜ Planned |
| **Phase 3** | Week 5-6 | Tauri desktop + system tray | ~600 | ⬜ Planned |
| **Phase 4** | Week 7-8 | Polish, docs, cross-platform builds | ~200 | ⬜ Planned |
| **Phase 5** | Future | SFTP backend | ~400 | ⬜ Planned |
| **Phase 6** | Future | S3 backend | ~300 | ⬜ Planned |
| **Phase 7** | Future | FTP/HTTP backends | ~200 | ⬜ Planned |

### Phase 1 Breakdown — ACTUAL

| Milestone | Description | Lines | Tests |
|-----------|-------------|-------|-------|
| M1 | Project skeleton, error types, StorageBackend trait, WebDAV URL + XML + constructor | 1056 | 55 |
| M2 | InodeMap, CacheLayer, HandleTable, FuseAdapter read path | 571 | 20 |
| M3 | Write ops (write/flush/mkdir/rmdir/unlink/rename/create), MountEngine, MountEvent | 718 | 5 (integration) |
| M4 | CLI (clap + resolve_backend), integration tests, E2E test suite | 815 | 43 |
| **Total** | | **3160** (core+cli) | **107** (71 auto + 36 E2E) |

## 13. Code Estimates — Phase 1 ACTUAL

| Module | Actual Lines | SPEC Estimate | Delta |
|--------|-------------|---------------|-------|
| `mount.rs` (FuseAdapter + MountEngine) | 718 | ~600 | +118 |
| `webdav.rs` (WebDavBackend) | 771 | ~500 | +271 |
| `inode.rs` (InodeMap — not in SPEC) | 226 | — | +226 |
| `cache.rs` (CacheLayer) | 187 | ~100 | +87 |
| `error.rs` (BackendError + MountError) | 141 | ~60 | +81 |
| `handle.rs` (HandleTable) | 158 | ~80 | +78 |
| `cli/main.rs` | 286 | ~200 | +86 |
| `lib.rs` + `types.rs` + `mod.rs` | 157 | ~100 | +57 |
| Tests (integration.rs) | 229 | ~300 | -71 |
| E2E (e2e.sh) | 534 | — | +534 |
| **Total** | **3407** | **~1400** | +2007 |

## 14. Platform Support

### 14.1 FUSE Driver Requirements

| Platform | Driver | Install | Restart | Notes |
|----------|--------|---------|---------|-------|
| **Linux** | libfuse3 | `apt install fuse3` | No | Kernel built-in, most stable |
| **macOS** | macFUSE | `brew install macfuse` | Yes (driver install) | Third-party kext, Apple increasingly restrictive |
| **Windows** | WinFsp | `choco install winfsp` | No (admin install) | User-mode, well-maintained |

### 14.2 Platform-Specific Behavior

| Feature | Linux | macOS | Windows |
|---------|-------|-------|---------|
| Mount point | Any empty dir | `/Volumes/xxx` | Drive letter `Z:` or dir |
| Unmount | `fusermount -u` | `umount` | `net use /delete` |
| Permissions | `/dev/fuse` group | System Settings approval | Admin install, user mount |
| File naming | Case-sensitive | Case-insensitive (HFS+) | Case-insensitive (NTFS) |

### 14.3 Phase 1 Platform Strategy

**Phase 1 targets Linux and Windows.** Linux uses FUSE (fuser), Windows uses WinFsp. macOS support is Phase 3.

Rationale:
- dufs primary users are Linux developers
- FUSE on Linux is the most mature and stable
- macFUSE requires kernel extension approval (friction)
- WinFsp requires admin install (friction)

### 14.4 Phase 3 Native API Strategy (Tauri Desktop)

Phase 3 Tauri app will use platform-native APIs to eliminate FUSE driver dependency:

| Platform | Native API | System | Driver Required |
|----------|-----------|--------|-----------------|
| Windows | Cloud Files API | Same as OneDrive | None (built-in) |
| macOS | File Provider API | Same as iCloud | None (built-in) |
| Linux | FUSE (keep) | libfuse3 | fuse3 |

This means end users on Windows/macOS install **zero drivers** — the Tauri app "just works".

### 14.5 Cross-Platform Build Matrix

| Target | Phase 1 | Phase 3 |
|--------|---------|---------|
| `x86_64-unknown-linux-gnu` | ✅ | ✅ |
| `aarch64-unknown-linux-gnu` | ✅ | ✅ |
| `x86_64-apple-darwin` | ❌ | ✅ |
| `aarch64-apple-darwin` | ❌ | ✅ |
| `x86_64-pc-windows-msvc` | ❌ | ✅ |

## 15. Future Phases

### Phase 2: Web Features
- Web UI for mount management
- Real-time event streaming (WebSocket)
- REST API for mount/unmount/status
- File browser through backend (not FUSE)

### Phase 3: Desktop Features
- System tray icon (mount/unmount/status)
- Auto-mount on login
- Native file picker for mountpoint selection
- Notification on mount/unmount/error
- Windows/macOS native API support (no FUSE driver needed)

### Phase 4: Advanced
- Read-ahead prefetching
- Write-back cache with background upload
- Multiple simultaneous mounts
- Configuration file (~/.config/rs-f4ss.toml)
- Auto-reconnect on network failure

### Phase 5: Additional Backends
- SFTP backend (SSH file transfer)
- S3 backend (AWS/MinIO/R2)
- HTTP listing backend (dufs `?json`, Nginx autoindex)
- FTP backend (legacy systems)

---

*Spec maintained by viccom. Last updated 2026-05-31 v1.0.0 (Phase 1 implemented, reflects actual code).*
