//! Windows-specific CLI helpers: status, unmount, Ctrl+C handler.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Validate mount point for Windows.
///
/// Drive letters (e.g. `Z:`, `X:`) are virtual — WinFsp creates them on mount.
/// Directory paths must exist and be actual directories.
fn is_drive_letter(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let s = s.trim_end_matches('\\').trim_end_matches('/');
    s.len() == 2 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1] == b':'
}

pub fn validate_mountpoint(path: &Path) -> Result<(), String> {
    if is_drive_letter(path) {
        return Ok(());
    }
    if !path.exists() {
        return Err(format!("Mount point does not exist: {}", path.display()));
    }
    if !path.is_dir() {
        return Err(format!(
            "Mount point is not a directory: {}",
            path.display()
        ));
    }
    Ok(())
}

/// Try to acquire exclusive serve lock using a PID file.
/// On Linux, flock is used for robustness; on Windows we use PID file + process check.
pub struct ServeLock {
    _private: (),
}

pub fn try_acquire_serve_lock() -> Result<ServeLock, String> {
    let dir = dirs::state_dir()
        .or_else(|| dirs::cache_dir())
        .unwrap_or_else(|| PathBuf::from(std::env::temp_dir()));
    let dir = dir.join("rs-f4ss");
    let _ = std::fs::create_dir_all(&dir);
    let pid_path = dir.join("serve.pid");

    // Check existing PID
    if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            // Check if process is still alive via tasklist
            let alive = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output()
                .map(|o| {
                    let out = String::from_utf8_lossy(&o.stdout);
                    out.contains(&pid.to_string()) && !out.contains("No tasks")
                })
                .unwrap_or(true);
            if alive {
                return Err(format!(
                    "Another rs-f4ss serve instance is already running (PID {pid}). \
                     Only one serve is allowed at a time."
                ));
            }
        }
        let _ = std::fs::remove_file(&pid_path);
    }

    std::fs::write(&pid_path, std::process::id().to_string())
        .map_err(|e| format!("Cannot write serve PID file: {e}"))?;

    Ok(ServeLock { _private: () })
}

/// List active rs-f4ss processes using PowerShell (wmic is deprecated).
pub fn get_active_mounts() -> Vec<(String, String)> {
    let output = match std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_Process -Filter \"Name='rs-f4ss.exe'\" | \
             Select-Object -ExpandProperty CommandLine",
        ])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut mounts = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if !line.contains("rs-f4ss") || line.contains("powershell") {
            continue;
        }
        // Parse: "rs-f4ss http://host:5000 Z: ..." or "path\rs-f4ss.exe ..."
        let parts: Vec<&str> = line.split_whitespace().collect();
        // Skip past the exe name (may contain spaces in path)
        let start = parts
            .iter()
            .position(|p| p.ends_with("rs-f4ss") || p.ends_with("rs-f4ss.exe"))
            .unwrap_or(0);
        if parts.len() > start + 2 && parts[start + 1].starts_with("http") {
            mounts.push((parts[start + 1].to_string(), parts[start + 2].to_string()));
        }
    }
    mounts
}

/// Unmount a WinFsp mount point by finding and terminating the serving process.
pub fn handle_unmount(mountpoint: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mount = mountpoint.trim_end_matches('\\').trim_end_matches('/');

    // Find rs-f4ss.exe processes and match by mountpoint argument
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "Get-CimInstance Win32_Process -Filter \"Name='rs-f4ss.exe'\" | \
                 Where-Object {{ $_.CommandLine -like '*{}*' }} | \
                 Select-Object -ExpandProperty ProcessId",
                mount.replace('\'', "''"),
            ),
        ])
        .output()?;

    let pids: Vec<u32> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();

    if pids.is_empty() {
        return Err(format!("No rs-f4ss process found for '{mountpoint}'").into());
    }

    for pid in &pids {
        let kill = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()?;
        if !kill.status.success() {
            tracing::warn!(
                "Failed to kill PID {pid}: {}",
                String::from_utf8_lossy(&kill.stderr)
            );
        }
    }

    println!("Unmounted {mount} (killed {} process(es))", pids.len());
    Ok(())
}

type UnmountFn = Arc<dyn Fn() + Send + Sync>;

/// Set up Ctrl+C handler for CLI mount mode.
///
/// On Windows, we store the unmount callback registered by mount_windows and
/// invoke it when Ctrl+C is received, which calls FspFileSystemStopDispatcher.
pub fn setup_ctrlc_handler(
    _mountpoint: PathBuf,
    unmount_cb: Arc<std::sync::Mutex<Option<UnmountFn>>>,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create signal runtime");
        rt.block_on(async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to listen for ctrl+c");
        });
        tracing::info!("Ctrl+C received, stopping WinFsp dispatcher...");
        if let Some(cb) = unmount_cb.lock().unwrap().as_ref() {
            cb();
        } else {
            tracing::warn!("Unmount callback not yet registered — process may not exit cleanly");
            // Fallback: just exit and let the process Drop handle cleanup
            std::process::exit(0);
        }
    });
}
