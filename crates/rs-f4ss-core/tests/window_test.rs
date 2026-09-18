//! Integration tests for the anchored window read model (window.rs).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use rs_f4ss_core::error::BackendError;
use rs_f4ss_core::window::{self, WindowState};

type FetchFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, BackendError>> + Send>>;
type CallLog = Arc<Mutex<Vec<(u64, u32)>>>;

/// Fetch spy: serves `content` sliced at (anchor, len) the way a backend
/// would (clamped to the content end, empty at/past it) and records every
/// call as (anchor, len).
fn fetch_spy(content: Vec<u8>) -> (CallLog, impl Fn(u64, u32) -> FetchFuture) {
    let calls: CallLog = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let content = Arc::new(content);
    let fetch = move |anchor: u64, len: u32| -> FetchFuture {
        let content = content.clone();
        let recorded = recorded.clone();
        Box::pin(async move {
            recorded.lock().unwrap().push((anchor, len));
            let start = anchor as usize;
            let end = (start + len as usize).min(content.len());
            if start >= content.len() {
                Ok(Vec::new())
            } else {
                Ok(content[start..end].to_vec())
            }
        })
    };
    (calls, fetch)
}

#[tokio::test]
async fn eof_read_returns_zero_and_fetches_nothing() {
    let mut st = WindowState::new(window::DEFAULT_READ_WINDOW);
    st.size = Some(100);
    let (calls, fetch) = fetch_spy(b"x".repeat(100));
    let out = window::read_at(&mut st, 100, 4096, fetch).await.unwrap();
    assert!(out.is_empty());
    assert!(calls.lock().unwrap().is_empty(), "no request past EOF");
}

#[tokio::test]
async fn window_clamps_fetch_to_eof() {
    let mut st = WindowState::new(window::DEFAULT_READ_WINDOW);
    st.size = Some(1024); // EOF 前 1 KiB 开 4 MiB 窗
    let (calls, fetch) = fetch_spy(vec![b'x'; 1024]);
    let out = window::read_at(&mut st, 0, 1024, fetch).await.unwrap();
    assert_eq!(out.len(), 1024);
    assert_eq!(calls.lock().unwrap()[0].1, 1024, "fetch len clamped to size");
}

#[tokio::test]
async fn reads_within_window_hit_cache() {
    // 一次 fetch 服务 3 次连续小读，calls.len()==1
    let mut st = WindowState::new(window::DEFAULT_READ_WINDOW);
    st.size = Some(4096);
    let (calls, fetch) = fetch_spy(vec![b'w'; 4096]);
    let a = window::read_at(&mut st, 0, 10, &fetch).await.unwrap();
    let b = window::read_at(&mut st, 10, 10, &fetch).await.unwrap();
    let c = window::read_at(&mut st, 20, 10, &fetch).await.unwrap();
    assert_eq!(a.len(), 10);
    assert_eq!(b.len(), 10);
    assert_eq!(c.len(), 10);
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn large_read_spans_multiple_windows() {
    // 9 MiB 读 → 3 次 fetch（4+4+1）
    const MIB: usize = 1024 * 1024;
    let total = 9 * MIB;
    let mut st = WindowState::new(window::DEFAULT_READ_WINDOW);
    st.size = Some(total as u64);
    let (calls, fetch) = fetch_spy(vec![7u8; total]);
    let out = window::read_at(&mut st, 0, total as u32, fetch).await.unwrap();
    assert_eq!(out.len(), total);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 3, "9 MiB read spans 4+4+1 windows");
    assert_eq!(calls[0].0, 0);
    assert_eq!(calls[0].1, 4 * MIB as u32);
    assert_eq!(calls[1].0, 4 * MIB as u64);
    assert_eq!(calls[1].1, 4 * MIB as u32);
    assert_eq!(calls[2].0, 8 * MIB as u64);
    assert_eq!(calls[2].1, MIB as u32);
}

#[tokio::test]
async fn never_serves_past_eof_when_backend_overdelivers() {
    // fetch 返回超量（500 字节）→ 服务端钳制，不越过已知 size
    let mut st = WindowState::new(window::DEFAULT_READ_WINDOW);
    st.size = Some(100);
    let fetch = move |_anchor: u64, _len: u32| {
        Box::pin(async { Ok(vec![b'o'; 500]) }) as FetchFuture
    };
    let out = window::read_at(&mut st, 0, 4096, fetch).await.unwrap();
    assert_eq!(out.len(), 100, "never serve past known EOF");
}

#[tokio::test]
async fn unknown_size_terminates_on_empty_fetch() {
    // size=None，fetch 返回空 → 短读返回已填字节
    let mut st = WindowState::new(window::DEFAULT_READ_WINDOW);
    let (calls, fetch) = fetch_spy(vec![b'u'; 2000]);
    let out = window::read_at(&mut st, 0, 4096, fetch).await.unwrap();
    assert_eq!(out.len(), 2000, "short read returns the bytes already filled");
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2, "second (empty) fetch terminates the read");
}

#[tokio::test]
async fn window_anchors_at_requested_offset() {
    // 直接从 offset=999_999 读 → 首个 fetch anchor==999_999
    let mut st = WindowState::new(window::DEFAULT_READ_WINDOW);
    let (calls, fetch) = fetch_spy(vec![b'a'; 1024 * 1024]);
    let out = window::read_at(&mut st, 999_999, 4096, fetch).await.unwrap();
    assert_eq!(out.len(), 4096);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 999_999, "window anchors at requested offset");
}
