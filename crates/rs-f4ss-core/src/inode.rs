//! Inode-to-path mapping for FUSE.
//!
//! Bidirectional map between inode numbers (`u64`) and filesystem paths.
//! Uses FNV-1a hash for deterministic inode number generation.
//!
//! Uses DashMap directly without an outer RwLock — DashMap is already concurrent-safe.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, error};

/// Root inode number (always 1 for FUSE).
pub const ROOT_INODE: u64 = 1;

/// FNV-1a offset basis.
const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;

/// FNV-1a prime.
const FNV_PRIME: u64 = 1_099_511_628_211;

/// Filesystem node type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    File,
    Dir,
}

/// Bidirectional inode-to-path map.
///
/// Uses FNV-1a hash for deterministic inode generation.
/// On collision, falls back to sequential allocation.
pub struct InodeMap {
    inode_to_path: DashMap<u64, Arc<Path>>,
    path_to_inode: DashMap<PathBuf, u64>,
    #[expect(dead_code)]
    root: PathBuf,
    /// Sequential fallback for inode collisions.
    next_sequential: AtomicU64,
}

impl InodeMap {
    /// Create a new map with the given root path.
    pub fn new(root: PathBuf) -> Self {
        let map = Self {
            inode_to_path: DashMap::new(),
            path_to_inode: DashMap::new(),
            root: root.clone(),
            next_sequential: AtomicU64::new(u64::MAX / 2),
        };
        map.inode_to_path
            .insert(ROOT_INODE, Arc::from(root.clone().into_boxed_path()));
        map.path_to_inode.insert(root, ROOT_INODE);
        map
    }

    /// Compute an inode for a path via FNV-1a hash.
    pub fn compute_inode(path: &Path, kind: NodeKind) -> u64 {
        let path_str = path.to_string_lossy();
        if path_str.is_empty() || path_str == "/" || path_str == "." {
            return ROOT_INODE;
        }
        let mut h: u64 = FNV_OFFSET;
        let kind_prefix = match kind {
            NodeKind::File => b"f:",
            NodeKind::Dir => b"d:",
        };
        for &b in kind_prefix.iter().chain(path_str.as_bytes().iter()) {
            h ^= u64::from(b);
            h = h.wrapping_mul(FNV_PRIME);
        }
        if h == ROOT_INODE {
            h = 2;
        }
        if h == 0 {
            h = u64::MAX;
        }
        h
    }

    /// Get or create an inode for a path.
    /// On hash collision, allocates a sequential inode.
    pub fn get_or_insert(&self, path: &Path, kind: NodeKind) -> u64 {
        if let Some(inode) = self.path_to_inode.get(path) {
            return *inode;
        }
        let mut inode = Self::compute_inode(path, kind);

        // Check for collision and resolve
        if let Some(existing) = self.inode_to_path.get(&inode) {
            if existing.value().as_ref() != path {
                error!(
                    inode,
                    new_path = %path.display(),
                    existing_path = %existing.value().display(),
                    "inode collision, falling back to sequential"
                );
                loop {
                    let fallback = self.next_sequential.fetch_sub(1, Ordering::Relaxed);
                    if !self.inode_to_path.contains_key(&fallback) {
                        inode = fallback;
                        break;
                    }
                }
            }
        }

        let path_buf = path.to_path_buf();
        self.path_to_inode.insert(path_buf.clone(), inode);
        self.inode_to_path
            .insert(inode, Arc::from(path_buf.into_boxed_path()));
        debug!(inode, path = %path.display(), ?kind, "inode registered");
        inode
    }

    /// Get a path by inode number.
    /// Returns Arc<Path> for O(1) clone — avoids allocation on every FUSE callback.
    pub fn get_path(&self, inode: u64) -> Option<Arc<Path>> {
        self.inode_to_path.get(&inode).map(|r| r.value().clone())
    }

    /// Remove a path from the map.
    pub fn remove(&self, path: &Path) {
        if let Some((_, inode)) = self.path_to_inode.remove(path) {
            self.inode_to_path.remove(&inode);
            debug!(inode, path = %path.display(), "inode mapping removed");
        }
    }

    /// Remove an inode by its number (for FUSE forget).
    pub fn remove_by_inode(&self, inode: u64) {
        if let Some((_, path)) = self.inode_to_path.remove(&inode) {
            self.path_to_inode.remove(path.as_ref());
            debug!(inode, "inode forgotten");
        }
    }

    /// Rename a path and all registered descendants.
    pub fn rename_subtree(&self, old_path: &Path, new_path: &Path) {
        let old_str = old_path.to_string_lossy();
        let prefix = if old_str.ends_with('/') {
            old_str.to_string()
        } else {
            format!("{}/", old_str)
        };

        let updates: Vec<_> = self
            .path_to_inode
            .iter()
            .filter_map(|entry| {
                let path = entry.key();
                let p = path.to_string_lossy();
                if p == old_str || p.starts_with(prefix.as_str()) {
                    let suffix = path.strip_prefix(old_path).unwrap_or(Path::new(""));
                    Some((*entry.value(), path.clone(), new_path.join(suffix)))
                } else {
                    None
                }
            })
            .collect();

        for (inode, old, new) in updates {
            self.path_to_inode.remove(&old);
            self.path_to_inode.insert(new.clone(), inode);
            self.inode_to_path
                .insert(inode, Arc::from(new.into_boxed_path()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_root_inode() {
        let map = InodeMap::new(PathBuf::from("/"));
        assert_eq!(map.get_path(ROOT_INODE).unwrap().as_os_str(), "/");
    }

    #[test]
    fn test_compute_inode_deterministic() {
        let path = Path::new("/file.txt");
        let a = InodeMap::compute_inode(path, NodeKind::File);
        let b = InodeMap::compute_inode(path, NodeKind::File);
        assert_eq!(a, b);
    }

    #[test]
    fn test_file_dir_different_inodes() {
        let path = Path::new("/name");
        let f = InodeMap::compute_inode(path, NodeKind::File);
        let d = InodeMap::compute_inode(path, NodeKind::Dir);
        assert_ne!(f, d);
    }

    #[test]
    fn test_get_or_insert() {
        let map = InodeMap::new(PathBuf::from("/"));
        let path = Path::new("/subdir/file.txt");
        let a = map.get_or_insert(path, NodeKind::File);
        let b = map.get_or_insert(path, NodeKind::File);
        assert_eq!(a, b);
        assert_eq!(map.get_path(a).unwrap().as_os_str(), path.as_os_str());
    }

    #[test]
    fn test_remove() {
        let map = InodeMap::new(PathBuf::from("/"));
        let path = Path::new("/file.txt");
        let inode = map.get_or_insert(path, NodeKind::File);
        map.remove(path);
        assert!(map.get_path(inode).is_none());
    }

    #[test]
    fn test_remove_by_inode() {
        let map = InodeMap::new(PathBuf::from("/"));
        let path = Path::new("/file.txt");
        let inode = map.get_or_insert(path, NodeKind::File);
        map.remove_by_inode(inode);
        assert!(map.get_path(inode).is_none());
    }

    #[test]
    fn test_rename_subtree() {
        let map = InodeMap::new(PathBuf::from("/"));
        let file = PathBuf::from("/old/a.md");
        let inode = map.get_or_insert(&file, NodeKind::File);
        map.rename_subtree(Path::new("/old"), Path::new("/new"));
        assert_eq!(map.get_path(inode).unwrap().as_os_str(), "/new/a.md");
    }
}
