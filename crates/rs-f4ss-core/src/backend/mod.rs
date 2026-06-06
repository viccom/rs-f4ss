#[cfg(any(feature = "webdav", feature = "http"))]
pub(crate) mod common;
#[cfg(feature = "http")]
pub mod http;
pub mod types;
#[cfg(feature = "webdav")]
pub mod webdav;

#[cfg(feature = "http")]
pub use http::HttpBackend;
pub use types::Entry;
#[cfg(feature = "webdav")]
pub use webdav::WebDavBackend;

use crate::error::BackendError;
use async_trait::async_trait;

/// Detect the storage protocol from a URL scheme.
pub fn detect_protocol(url: &str) -> String {
    let lower = url.to_lowercase();
    if lower.starts_with("s3://") {
        "s3".to_string()
    } else if lower.starts_with("static://") || lower.starts_with("statics://") {
        "http".to_string()
    } else if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("webdav://")
        || lower.starts_with("webdavs://")
    {
        "webdav".to_string()
    } else if let Some(i) = lower.find("://") {
        url[..i].to_string()
    } else {
        "unknown".to_string()
    }
}

#[async_trait]
pub trait StorageBackend: Send + Sync + 'static {
    fn protocol(&self) -> &str;
    fn server_addr(&self) -> &str;
    fn is_read_only(&self) -> bool {
        false
    }

    async fn list(&self, path: &str) -> Result<Vec<Entry>, BackendError>;
    async fn stat(&self, path: &str) -> Result<Entry, BackendError>;
    async fn read(&self, path: &str, offset: u64, size: u32) -> Result<Vec<u8>, BackendError>;
    async fn write(&self, path: &str, data: &[u8]) -> Result<(), BackendError>;
    async fn mkdir(&self, path: &str) -> Result<(), BackendError>;
    async fn delete(&self, path: &str) -> Result<(), BackendError>;
    async fn rename(&self, from: &str, to: &str) -> Result<(), BackendError>;

    async fn ping(&self) -> Result<bool, BackendError> {
        Ok(true)
    }
}

#[async_trait]
impl StorageBackend for Box<dyn StorageBackend> {
    fn protocol(&self) -> &str {
        (**self).protocol()
    }
    fn server_addr(&self) -> &str {
        (**self).server_addr()
    }
    fn is_read_only(&self) -> bool {
        (**self).is_read_only()
    }

    async fn list(&self, path: &str) -> Result<Vec<Entry>, BackendError> {
        (**self).list(path).await
    }
    async fn stat(&self, path: &str) -> Result<Entry, BackendError> {
        (**self).stat(path).await
    }
    async fn read(&self, path: &str, offset: u64, size: u32) -> Result<Vec<u8>, BackendError> {
        (**self).read(path, offset, size).await
    }
    async fn write(&self, path: &str, data: &[u8]) -> Result<(), BackendError> {
        (**self).write(path, data).await
    }
    async fn mkdir(&self, path: &str) -> Result<(), BackendError> {
        (**self).mkdir(path).await
    }
    async fn delete(&self, path: &str) -> Result<(), BackendError> {
        (**self).delete(path).await
    }
    async fn rename(&self, from: &str, to: &str) -> Result<(), BackendError> {
        (**self).rename(from, to).await
    }
    async fn ping(&self) -> Result<bool, BackendError> {
        (**self).ping().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_protocol() {
        assert_eq!(detect_protocol("http://host"), "webdav");
        assert_eq!(detect_protocol("https://host"), "webdav");
        assert_eq!(detect_protocol("webdav://host"), "webdav");
        assert_eq!(detect_protocol("webdavs://host"), "webdav");
        assert_eq!(detect_protocol("static://host"), "http");
        assert_eq!(detect_protocol("statics://host"), "http");
        assert_eq!(detect_protocol("s3://bucket"), "s3");
        assert_eq!(detect_protocol("ftp://host"), "ftp");
        assert_eq!(detect_protocol("no-scheme"), "unknown");
    }
}
