//! 锚定窗口读模型（移植自 cydrive K34）。
//!
//! 每句柄一个窗口，锚定在请求 offset："开窗即预取，无投机 read-ahead"。
//! size 已知时全链路钳制到 EOF：offset ≥ size 的读直接返回空、零网络，
//! 从结构上消灭"416 → 整文件下载"。

use crate::error::BackendError;

/// 默认窗口 4 MiB（cydrive DEFAULT_READ_WINDOW 同值）。若高延迟链路顺序
/// 吞吐回退 >20%（Task 7 基准），上调此常量至 16 MiB 再测。
pub const DEFAULT_READ_WINDOW: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ReadWindow {
    pub start: u64,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub struct WindowState {
    /// 已知文件大小（驱动一切 EOF 钳制）；首次 stat 成功前为 None。
    pub size: Option<u64>,
    pub window: Option<ReadWindow>,
    pub window_size: usize,
}

impl WindowState {
    pub fn new(window_size: usize) -> Self {
        Self {
            size: None,
            window: None,
            window_size: window_size.max(4096),
        }
    }
    fn covers(&self, p: u64) -> bool {
        match &self.window {
            Some(w) => p >= w.start && p - w.start < w.data.len() as u64,
            None => false,
        }
    }
}

/// 读 [offset, offset+size)。窗口 miss 时在请求 offset 处开窗（钳到 EOF），
/// 窗口命中零网络。返回短读（可能为空）表示 EOF 或后端停滞。
pub async fn read_at<F, Fut>(
    state: &mut WindowState,
    offset: u64,
    size: u32,
    fetch: F,
) -> Result<Vec<u8>, BackendError>
where
    F: Fn(u64, u32) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, BackendError>>,
{
    let want = size as u64;
    let mut out: Vec<u8> = Vec::with_capacity(size as usize);
    let mut pos = offset;
    while (out.len() as u64) < want {
        if let Some(sz) = state.size {
            if pos >= sz {
                break; // EOF：不发任何请求
            }
        }
        if !state.covers(pos) {
            let mut len = state.window_size as u64;
            if let Some(sz) = state.size {
                len = len.min(sz - pos); // 开窗即钳到 EOF
            }
            let data = fetch(pos, len as u32).await?;
            if data.is_empty() {
                // 后端在 EOF 之下给了空窗：短读优于在 dispatcher 线程上自旋
                break;
            }
            state.window = Some(ReadWindow { start: pos, data });
        }
        let w = state.window.as_ref().expect("window filled above");
        let consumed = (pos - w.start) as usize;
        let mut n = (w.data.len() - consumed).min((want - out.len() as u64) as usize);
        if let Some(sz) = state.size {
            n = n.min((sz - pos) as usize); // 后端超发也不越过 EOF
        }
        out.extend_from_slice(&w.data[consumed..consumed + n]);
        pos += n as u64;
    }
    Ok(out)
}
