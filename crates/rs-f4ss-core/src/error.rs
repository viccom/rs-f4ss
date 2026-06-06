use thiserror::Error;

#[derive(Error, Debug)]
pub enum BackendError {
    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Read-only backend")]
    ReadOnly,

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Operation not supported: {0}")]
    NotSupported(String),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Map BackendError to `std::io::Error` (platform-agnostic).
pub fn map_to_io_error(err: &BackendError) -> std::io::Error {
    use std::io::ErrorKind;
    let kind = match err {
        BackendError::NotFound(_) => ErrorKind::NotFound,
        BackendError::PermissionDenied(_) => ErrorKind::PermissionDenied,
        BackendError::ConnectionFailed(_) => ErrorKind::ConnectionRefused,
        BackendError::ReadOnly => ErrorKind::PermissionDenied,
        BackendError::ProtocolError(_) => ErrorKind::InvalidData,
        BackendError::InvalidPath(_) => ErrorKind::InvalidInput,
        BackendError::NotSupported(_) => ErrorKind::Other,
        BackendError::Internal(_) => ErrorKind::Other,
    };
    std::io::Error::new(kind, err.to_string())
}

#[derive(Error, Debug)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_not_found_display() {
        let err = BackendError::NotFound("x".into());
        assert_eq!(err.to_string(), "Not found: x");
    }

    #[test]
    fn test_permission_display() {
        let err = BackendError::PermissionDenied("y".into());
        assert_eq!(err.to_string(), "Permission denied: y");
    }

    #[test]
    fn test_connection_display() {
        let err = BackendError::ConnectionFailed("z".into());
        assert_eq!(err.to_string(), "Connection failed: z");
    }

    #[test]
    fn test_readonly_display() {
        let err = BackendError::ReadOnly;
        assert_eq!(err.to_string(), "Read-only backend");
    }

    #[test]
    fn test_mount_error_fuse() {
        let err = MountError::FuseError("e".into());
        assert_eq!(err.to_string(), "FUSE error: e");
    }

    #[test]
    fn test_mount_error_auth() {
        let err = MountError::AuthFailed;
        assert_eq!(err.to_string(), "Authentication failed");
    }

    #[test]
    fn test_io_error_mapping_not_found() {
        let err = map_to_io_error(&BackendError::NotFound("test".into()));
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn test_io_error_mapping_readonly() {
        let err = map_to_io_error(&BackendError::ReadOnly);
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn test_io_error_mapping_invalid_path() {
        let err = map_to_io_error(&BackendError::InvalidPath("bad".into()));
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }
}
