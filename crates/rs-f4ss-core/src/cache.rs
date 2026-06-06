use moka::future::Cache;
use std::path::Path;
use std::time::Duration;

use crate::backend::Entry;

#[derive(Clone)]
pub struct CachedAttr {
    pub entry: Entry,
}

#[derive(Clone)]
pub struct CachedChildren {
    pub entries: Vec<Entry>,
}

#[derive(Clone)]
pub struct CacheLayer {
    attrs: Cache<String, CachedAttr>,
    children: Cache<String, CachedChildren>,
}

impl CacheLayer {
    pub fn new(ttl: Duration, max_size: u64) -> Self {
        let attrs = Cache::builder()
            .time_to_live(ttl)
            .max_capacity(max_size)
            .build();

        let children = Cache::builder()
            .time_to_live(ttl)
            .max_capacity(max_size)
            .build();

        Self { attrs, children }
    }

    pub async fn get_attr(&self, path: &str) -> Option<CachedAttr> {
        self.attrs.get(path).await
    }

    pub async fn set_attr(&self, path: &str, attr: CachedAttr) {
        self.attrs.insert(path.to_string(), attr).await;
    }

    pub async fn get_children(&self, path: &str) -> Option<CachedChildren> {
        self.children.get(path).await
    }

    pub async fn set_children(&self, path: &str, children: CachedChildren) {
        self.children.insert(path.to_string(), children).await;
    }

    pub async fn invalidate(&self, path: &str) {
        self.attrs.invalidate(path).await;
        self.children.invalidate(path).await;
    }

    pub async fn invalidate_parent(&self, path: &str) {
        if let Some(parent) = Path::new(path).parent() {
            let parent_str = parent.to_string_lossy().to_string();
            let key = if parent_str.is_empty() {
                "/"
            } else {
                &parent_str
            };
            self.children.invalidate(key).await;
        }
    }

    pub async fn clear(&self) {
        self.attrs.invalidate_all();
        self.children.invalidate_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn create_test_entry(name: &str, dir: bool) -> Entry {
        Entry {
            path: format!("/test/{name}"),
            name: name.to_string(),
            dir,
            size: 100,
            mtime: SystemTime::UNIX_EPOCH,
        }
    }

    #[tokio::test]
    async fn test_cache_set_get_attr() {
        let cache = CacheLayer::new(Duration::from_secs(60), 100);
        let entry = create_test_entry("file.txt", false);
        let attr = CachedAttr {
            entry: entry.clone(),
        };

        cache.set_attr("/test/file.txt", attr).await;
        let cached = cache.get_attr("/test/file.txt").await;
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().entry.name, "file.txt");
    }

    #[tokio::test]
    async fn test_cache_miss() {
        let cache = CacheLayer::new(Duration::from_secs(60), 100);
        let result = cache.get_attr("/nonexistent").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_cache_invalidate() {
        let cache = CacheLayer::new(Duration::from_secs(60), 100);
        let entry = create_test_entry("file.txt", false);
        let attr = CachedAttr { entry };

        cache.set_attr("/test/file.txt", attr).await;
        cache.invalidate("/test/file.txt").await;
        let result = cache.get_attr("/test/file.txt").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_cache_invalidate_parent() {
        let cache = CacheLayer::new(Duration::from_secs(60), 100);
        let children = CachedChildren {
            entries: vec![
                create_test_entry("file1.txt", false),
                create_test_entry("file2.txt", false),
            ],
        };

        cache.set_children("/a", children).await;
        cache.invalidate_parent("/a/b").await;
        let result = cache.get_children("/a").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_cache_clear() {
        let cache = CacheLayer::new(Duration::from_secs(60), 100);

        cache
            .set_attr(
                "/file1",
                CachedAttr {
                    entry: create_test_entry("file1", false),
                },
            )
            .await;
        cache
            .set_attr(
                "/file2",
                CachedAttr {
                    entry: create_test_entry("file2", false),
                },
            )
            .await;

        cache.clear().await;

        assert!(cache.get_attr("/file1").await.is_none());
        assert!(cache.get_attr("/file2").await.is_none());
    }

    #[tokio::test]
    async fn test_cache_set_get_children() {
        let cache = CacheLayer::new(Duration::from_secs(60), 100);
        let children = CachedChildren {
            entries: vec![
                create_test_entry("file1.txt", false),
                create_test_entry("dir1", true),
            ],
        };

        cache.set_children("/test", children).await;
        let cached = cache.get_children("/test").await;
        assert!(cached.is_some());
        let entries = cached.unwrap().entries;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "file1.txt");
        assert_eq!(entries[1].name, "dir1");
    }
}
