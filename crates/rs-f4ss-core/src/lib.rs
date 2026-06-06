pub mod backend;
pub mod cache;
pub mod error;
pub mod handle;
pub mod inode;
pub mod mount;
pub mod prefetch;

#[cfg(target_os = "linux")]
pub mod mount_linux;
#[cfg(target_os = "windows")]
pub mod mount_windows;

#[cfg(feature = "api")]
pub mod api;
#[cfg(feature = "api")]
pub mod manager;
#[cfg(any(feature = "api", feature = "serve"))]
pub mod persistence;

#[cfg(feature = "serve")]
pub mod server;
#[cfg(feature = "serve")]
pub mod share_manager;

pub use backend::{detect_protocol, Entry, StorageBackend};
pub use error::{BackendError, MountError};
pub use handle::HandleTable;
pub use inode::{InodeMap, NodeKind, ROOT_INODE};
pub use mount::{FuseAdapter, MountConfig, MountEngine, MountEvent, MountStatus};

#[cfg(feature = "webdav")]
pub use backend::webdav::WebDavBackend;

#[cfg(feature = "http")]
pub use backend::http::HttpBackend;

#[cfg(feature = "api")]
pub use manager::{MountEntry, MountInfo, MountManager, MountState};

#[cfg(feature = "serve")]
pub use share_manager::{ShareConfig, ShareInfo, ShareManager, ShareState};
