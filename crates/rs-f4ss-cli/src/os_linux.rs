//! Linux-specific CLI helpers: status, unmount, Ctrl+C handler, daemon.

use std::fs;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Validate mount point for Linux: must be an existing directory.
pub fn validate_mountpoint(path: &Path) -> Result<(), String> {
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

/// Parse `/proc/mounts` for active rs-f4ss entries.
pub fn get_active_mounts() -> Vec<(String, String)> {
    let content = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    content
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[0].starts_with("rs-f4ss") {
                Some((parts[0].to_string(), parts[1].to_string()))
            } else {
                None
            }
        })
        .collect()
}

/// Try to acquire exclusive serve lock. Returns a guard that holds the lock until dropped.
pub struct ServeLock {
    _file: std::fs::File,
}

pub fn try_acquire_serve_lock() -> Result<ServeLock, String> {
    let path = state_dir().join("serve.lock");
    let file =
        std::fs::File::create(&path).map_err(|e| format!("Cannot create serve lock file: {e}"))?;
    let fd = file.as_raw_fd();
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        return Err(
            "Another rs-f4ss serve instance is already running. Only one serve is allowed at a time.".to_string()
        );
    }
    Ok(ServeLock { _file: file })
}

pub fn handle_unmount(mountpoint: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mp = PathBuf::from(mountpoint);
    if !mountpoint_is_active(&mp) {
        eprintln!("Not a rs-f4ss: {mountpoint}");
        std::process::exit(1);
    }
    let status = Command::new("fusermount")
        .args(["-u", mountpoint])
        .status()?;
    if status.success() {
        println!("Unmounted {mountpoint}");
        Ok(())
    } else {
        let lazy = Command::new("umount").args(["-l", mountpoint]).status()?;
        if lazy.success() {
            println!("Lazy-unmounted {mountpoint}");
            Ok(())
        } else {
            Err(format!("Failed to unmount {mountpoint}").into())
        }
    }
}

pub fn setup_ctrlc_handler(mountpoint: PathBuf) {
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
        tracing::info!("Ctrl+C received, unmounting {}...", mountpoint.display());
        let out = Command::new("fusermount")
            .args(["-u", &mountpoint.to_string_lossy()])
            .output();
        match &out {
            Ok(s) if s.status.success() => {
                tracing::info!("Graceful unmount completed");
            }
            _ => {
                tracing::warn!("fusermount -u failed, trying lazy unmount...");
                let _ = Command::new("umount")
                    .args(["-l", &mountpoint.to_string_lossy()])
                    .output();
            }
        }
    });
}

fn mountpoint_is_active(mp: &Path) -> bool {
    get_active_mounts().iter().any(|(_, m)| Path::new(m) == mp)
}

/// State directory for PID and log files.
fn state_dir() -> PathBuf {
    let base = dirs::state_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let dir = base.join("rs-f4ss");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// PID file path for a given mount point.
pub fn pid_file(mountpoint: &Path) -> PathBuf {
    let name = mountpoint.to_string_lossy().replace('/', "_");
    state_dir().join(format!("mount{name}.pid"))
}

/// Log file path for a given mount point.
pub fn log_file(mountpoint: &Path) -> PathBuf {
    let name = mountpoint.to_string_lossy().replace('/', "_");
    state_dir().join(format!("mount{name}.log"))
}

/// Daemonize: fork to background, redirect stdout/stderr to log file, write PID.
/// Returns `Ok(())` in the child, parent exits the process.
pub fn daemonize(mountpoint: &Path) -> Result<(), String> {
    let log_path = log_file(mountpoint);
    let pid_path = pid_file(mountpoint);

    // Fork
    match unsafe { libc::fork() } {
        -1 => Err(format!("fork failed: {}", std::io::Error::last_os_error())),
        0 => {
            // Child: create new session, detach from terminal
            unsafe { libc::setsid() };

            // Redirect stdout/stderr to log file
            if let Ok(log_file) = fs::File::create(&log_path) {
                let fd = log_file.as_raw_fd();
                unsafe {
                    libc::dup2(fd, 1);
                    libc::dup2(fd, 2);
                }
            }
            // Close stdin
            let devnull = fs::File::open("/dev/null").unwrap();
            unsafe {
                libc::dup2(devnull.as_raw_fd(), 0);
            }

            // Write PID
            let pid = unsafe { libc::getpid() };
            let _ = fs::write(&pid_path, pid.to_string());

            Ok(())
        }
        pid => {
            // Parent: wait briefly for child to start, then exit
            unsafe {
                libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
            }
            println!("Started in background (PID {pid})");
            println!("  log:  {}", log_path.display());
            println!("  pid:  {}", pid_path.display());
            println!("  stop: rs-f4ss unmount {}", mountpoint.display());
            std::process::exit(0);
        }
    }
}
