use std::sync::{Arc, RwLock};

/// A typed snapshot of the current progress state.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProgressSnapshot {
    pub active: bool,
    pub phase: String,
    /// Download progress as 0-100.
    pub percent: u32,
    pub downloaded: u64,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug)]
struct ProgressInner {
    active: bool,
    phase: String,
    downloaded: u64,
    total: u64,
    error: Option<String>,
}

/// Thread-safe progress tracker for download operations.
#[derive(Debug, Clone)]
pub struct ProgressState {
    inner: Arc<RwLock<ProgressInner>>,
}

impl ProgressState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(ProgressInner {
                active: false,
                phase: String::new(),
                downloaded: 0,
                total: 0,
                error: None,
            })),
        }
    }

    /// Returns a typed copy of the current state.
    pub fn snapshot(&self) -> ProgressSnapshot {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let percent = if inner.total > 0 {
            (inner.downloaded * 100 / inner.total) as u32
        } else {
            0
        };
        ProgressSnapshot {
            active: inner.active,
            phase: inner.phase.clone(),
            percent,
            downloaded: inner.downloaded,
            total: inner.total,
            error: inner.error.clone(),
        }
    }

    pub(crate) fn start(&self) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.active = true;
        inner.phase = "downloading".into();
        inner.downloaded = 0;
        inner.total = 0;
        inner.error = None;
    }

    pub(crate) fn set_progress(&self, downloaded: u64, total: u64) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.downloaded = downloaded;
        inner.total = total;
    }

    pub(crate) fn set_done(&self) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.active = false;
        inner.phase = "done".into();
        inner.error = None;
    }

    pub(crate) fn set_error(&self, err: &str) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.active = false;
        inner.phase = "error".into();
        inner.error = Some(err.to_string());
    }
}

impl Default for ProgressState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        let ps = ProgressState::new();
        let s = ps.snapshot();
        assert!(!s.active);
        assert_eq!(s.phase, "");
        assert_eq!(s.percent, 0);
        assert_eq!(s.downloaded, 0);
        assert_eq!(s.total, 0);
        assert!(s.error.is_none());
    }

    #[test]
    fn test_progress_lifecycle() {
        let ps = ProgressState::new();

        ps.start();
        let s = ps.snapshot();
        assert!(s.active);
        assert_eq!(s.phase, "downloading");

        ps.set_progress(50, 100);
        let s = ps.snapshot();
        assert_eq!(s.percent, 50);
        assert_eq!(s.downloaded, 50);
        assert_eq!(s.total, 100);

        ps.set_done();
        let s = ps.snapshot();
        assert!(!s.active);
        assert_eq!(s.phase, "done");
    }

    #[test]
    fn test_error_state() {
        let ps = ProgressState::new();
        ps.start();
        ps.set_error("network timeout");
        let s = ps.snapshot();
        assert!(!s.active);
        assert_eq!(s.phase, "error");
        assert_eq!(s.error.as_deref(), Some("network timeout"));
    }

    #[test]
    fn test_set_done_clears_error() {
        let ps = ProgressState::new();
        ps.start();
        ps.set_error("transient");
        ps.start(); // simulate retry
        ps.set_done();
        let s = ps.snapshot();
        assert!(!s.active);
        assert_eq!(s.phase, "done");
        assert!(s.error.is_none());
    }
}
