use std::path::Path;

use crate::error::Error;

/// Restart the current program.
///
/// Uses the cached executable path from a prior `update()` call if available.
/// On Linux, `/proc/self/exe` may point to a deleted `.old` file after binary
/// replacement, so the cached path is more reliable.
///
/// - Unix: replaces the current process via `execv` (PID preserved)
/// - Windows: spawns a new process with `CREATE_NEW_PROCESS_GROUP`,
///   waits 2s for health check, then exits
pub fn restart(exe_path: &Path) -> Result<(), Error> {
    if exe_path.as_os_str().is_empty() {
        return Err(Error::NoCachedExePath);
    }
    #[cfg(unix)]
    {
        unix_restart(exe_path)
    }
    #[cfg(windows)]
    {
        windows_restart(exe_path)
    }
}

#[cfg(unix)]
fn unix_restart(exe_path: &Path) -> Result<(), Error> {
    use std::os::unix::process::CommandExt;
    // execv replaces the current process — does not return on success
    let err = std::process::Command::new(exe_path)
        .args(std::env::args().skip(1))
        .exec();
    // exec only returns on error
    Err(Error::Io(err))
}

#[cfg(windows)]
fn windows_restart(exe_path: &Path) -> Result<(), Error> {
    use std::os::windows::process::CommandExt;

    let mut child = std::process::Command::new(exe_path)
        .args(std::env::args().skip(1))
        .creation_flags(0x00000200) // CREATE_NEW_PROCESS_GROUP
        .spawn()?;

    // 2s health check via try_wait polling. If the new process exits
    // immediately (DLL missing, config error), report it instead of
    // silently disappearing. try_wait avoids the is_finished/join race
    // and the leaked waiter thread from the previous design.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let poll = std::time::Duration::from_millis(100);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("new process exited during health check: {}", status),
                    )));
                }
                // Successful exit during health check — old process also exits.
                std::process::exit(0);
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(poll);
            }
            Err(e) => return Err(Error::Io(e)),
        }
    }

    std::process::exit(0);
}
