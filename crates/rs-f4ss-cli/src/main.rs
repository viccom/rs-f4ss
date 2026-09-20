use clap::{Parser, Subcommand};
#[cfg(feature = "webdav")]
use rs_f4ss_core::WebDavBackend;
use rs_f4ss_core::{MountConfig, MountEngine, MountEvent, StorageBackend};
use std::path::PathBuf;
#[cfg(target_os = "windows")]
use std::sync::Arc;
use std::time::Duration;

#[cfg(target_os = "linux")]
mod os_linux;
#[cfg(target_os = "linux")]
use os_linux as os;

#[cfg(target_os = "windows")]
mod os_windows;
#[cfg(target_os = "windows")]
use os_windows as os;

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "rs-f4ss")]
#[command(about = "Mount remote file servers as local filesystems")]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    // ── Mount positional args (used when no subcommand) ──
    #[arg(help = "Remote server URL")]
    url: Option<String>,

    #[arg(help = "Local mount point")]
    mountpoint: Option<String>,

    // ── Mount options ──
    #[arg(short, long, help = "HTTP Basic auth username")]
    user: Option<String>,

    #[arg(short, long, help = "HTTP Basic auth password")]
    pass: Option<String>,

    #[arg(long, help = "Read password from file")]
    pass_file: Option<String>,

    #[arg(short, long, help = "Mount as read-only")]
    read_only: bool,

    #[arg(long, default_value = "60", help = "Metadata cache TTL in seconds")]
    cache_ttl: u64,

    #[arg(long, default_value = "256", help = "Max cache entries")]
    cache_size: usize,

    #[arg(short, long, help = "Run in foreground")]
    foreground: bool,

    #[arg(long, help = "Allow other users to access mount")]
    allow_other: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Show active rs-f4ss mount points
    Status,
    /// Unmount a rs-f4ss mount point
    Unmount {
        #[arg(help = "Mount point to unmount")]
        mountpoint: String,
    },
    /// Start REST API server for dynamic mount/share management
    Serve {
        #[arg(
            long,
            help = "Listen address [default: config file listen or 0.0.0.0:8080]"
        )]
        listen: Option<String>,
        #[arg(long, help = "Config file path (default: platform config dir)")]
        config: Option<String>,
        #[arg(long, help = "Stop the running serve instance via its API")]
        stop: bool,
        #[arg(
            long,
            env = "RS_F4SS_API_USER",
            help = "API auth username (used by --stop)"
        )]
        api_user: Option<String>,
        #[arg(
            long,
            env = "RS_F4SS_API_PASS",
            help = "API auth password (used by --stop)"
        )]
        api_pass: Option<String>,
    },
    /// Manage mount configs (via API)
    Mount {
        #[command(subcommand)]
        action: MountAction,
        #[arg(long, env = "RS_F4SS_API_USER", help = "API auth username")]
        api_user: Option<String>,
        #[arg(long, env = "RS_F4SS_API_PASS", help = "API auth password")]
        api_pass: Option<String>,
    },
    /// Manage file sharing (via API or standalone)
    Share {
        #[command(subcommand)]
        action: ShareAction,
        #[arg(long, env = "RS_F4SS_API_USER", help = "API auth username")]
        api_user: Option<String>,
        #[arg(long, env = "RS_F4SS_API_PASS", help = "API auth password")]
        api_pass: Option<String>,
    },
    /// Self-update the running binary
    #[cfg(feature = "selfupdate")]
    Update {
        #[command(subcommand)]
        action: UpdateAction,
        /// Override the manifest URL (default: GitHub releases/latest/download/latest.json)
        #[arg(long, env = "RS_F4SS_UPDATE_URL")]
        manifest_url: Option<String>,
    },
}

#[cfg(feature = "selfupdate")]
#[derive(Subcommand)]
enum UpdateAction {
    /// Show the current version, manifest URL, and platform
    Version,
    /// Check for a newer release without applying it
    Check,
    /// Download, install, and restart the running binary in place
    Apply {
        /// Skip the restart step (binary is replaced, you restart manually)
        #[arg(long)]
        no_restart: bool,
    },
}

#[derive(Subcommand)]
enum MountAction {
    /// List mount configs and status
    List {
        #[arg(
            long,
            default_value = "http://localhost:8080",
            help = "API server address"
        )]
        api: String,
    },
    /// Add a new mount config
    Add {
        #[arg(help = "Mount ID")]
        id: String,
        #[arg(long, help = "Remote server URL")]
        url: String,
        #[arg(long, help = "Local mount point path")]
        path: String,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        pass: Option<String>,
        #[arg(long, default_value_t = false)]
        read_only: bool,
        #[arg(long, default_value_t = 60)]
        cache_ttl: u64,
        #[arg(long, default_value_t = 256)]
        cache_size: usize,
        #[arg(
            long,
            default_value = "http://localhost:8080",
            help = "API server address"
        )]
        api: String,
    },
    /// Stop and delete a mount config
    Del {
        #[arg(help = "Mount ID")]
        id: String,
        #[arg(
            long,
            default_value = "http://localhost:8080",
            help = "API server address"
        )]
        api: String,
    },
    /// Start a mount by ID
    Start {
        #[arg(help = "Mount ID")]
        id: String,
        #[arg(
            long,
            default_value = "http://localhost:8080",
            help = "API server address"
        )]
        api: String,
    },
    /// Stop a mount by ID
    Stop {
        #[arg(help = "Mount ID")]
        id: String,
        #[arg(
            long,
            default_value = "http://localhost:8080",
            help = "API server address"
        )]
        api: String,
    },
}

#[derive(Subcommand)]
enum ShareAction {
    /// Start a standalone file sharing server (no API needed)
    Serve {
        #[arg(help = "Local directory to share")]
        path: String,
        #[arg(
            long,
            default_value = "127.0.0.1:8080",
            help = "Listen address (use 0.0.0.0:8080 to expose on the network)"
        )]
        listen: String,
        #[arg(short, long, help = "HTTP Basic Auth username")]
        user: Option<String>,
        #[arg(short, long, help = "HTTP Basic Auth password")]
        pass: Option<String>,
        #[arg(long, help = "Read-only mode (no upload/delete)")]
        read_only: bool,
    },
    /// List share configs and status (via API)
    List {
        #[arg(
            long,
            default_value = "http://localhost:8080",
            help = "API server address"
        )]
        api: String,
    },
    /// Add a share config (via API)
    Add {
        #[arg(help = "Share ID")]
        id: String,
        #[arg(long, help = "Local directory to share")]
        path: String,
        #[arg(long, default_value = "0.0.0.0:8081", help = "Listen address")]
        listen: String,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        pass: Option<String>,
        #[arg(long, default_value_t = false)]
        read_only: bool,
        #[arg(
            long,
            default_value = "http://localhost:8080",
            help = "API server address"
        )]
        api: String,
    },
    /// Delete a share config (via API)
    Del {
        #[arg(help = "Share ID")]
        id: String,
        #[arg(
            long,
            default_value = "http://localhost:8080",
            help = "API server address"
        )]
        api: String,
    },
    /// Start a share by ID (via API)
    Start {
        #[arg(help = "Share ID")]
        id: String,
        #[arg(
            long,
            default_value = "http://localhost:8080",
            help = "API server address"
        )]
        api: String,
    },
    /// Stop a share by ID (via API)
    Stop {
        #[arg(help = "Share ID")]
        id: String,
        #[arg(
            long,
            default_value = "http://localhost:8080",
            help = "API server address"
        )]
        api: String,
    },
}

// ---------------------------------------------------------------------------
// Backend resolution
// ---------------------------------------------------------------------------

fn resolve_backend(
    url: &str,
    read_only: bool,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<Box<dyn StorageBackend>, String> {
    let protocol = rs_f4ss_core::detect_protocol(url);

    match protocol.as_str() {
        "webdav" => {
            #[cfg(feature = "webdav")]
            {
                let backend = WebDavBackend::from_url(url, read_only, username, password)?;
                Ok(Box::new(backend))
            }
            #[cfg(not(feature = "webdav"))]
            #[cfg(feature = "http")]
            {
                // No webdav feature — try HTTP static backend
                let backend = rs_f4ss_core::HttpBackend::from_url(
                    url, read_only, username, password,
                )?;
                Ok(Box::new(backend))
            }
            #[cfg(not(any(feature = "webdav", feature = "http")))]
            {
                let _ = (read_only, username, password);
                Err("WebDAV protocol requires the 'webdav' feature".to_string())
            }
        }
        "http" => {
            #[cfg(feature = "http")]
            {
                let backend = rs_f4ss_core::HttpBackend::from_url(
                    url, read_only, username, password,
                )?;
                Ok(Box::new(backend))
            }
            #[cfg(not(feature = "http"))]
            {
                Err("HTTP backend requires the 'http' feature".to_string())
            }
        }
        "s3" | "sftp" | "ftp" => Err(format!("Unsupported protocol: {protocol}")),
        "unknown" => Err(
            "Invalid URL: must include scheme (http://, https://, static://, statics://, webdav://, or webdavs://)"
                .to_string(),
        ),
        _ => Err(format!("Unsupported protocol: {protocol}")),
    }
}

// ---------------------------------------------------------------------------
// status / unmount — platform-delegated
// ---------------------------------------------------------------------------

fn handle_status() -> Result<(), Box<dyn std::error::Error>> {
    let mounts = os::get_active_mounts();
    if mounts.is_empty() {
        println!("No active rs-f4ss mounts.");
        return Ok(());
    }
    println!("Active mounts:");
    for (_src, mp) in &mounts {
        println!("  {}", mp);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Self-update
// ---------------------------------------------------------------------------

#[cfg(feature = "selfupdate")]
fn build_default_updater() -> Option<rs_f4ss_core::selfupdate::SelfUpdater> {
    use rs_f4ss_core::selfupdate::{SelfUpdateConfig, SelfUpdater};
    let config = SelfUpdateConfig::from_env();
    match SelfUpdater::new(env!("CARGO_PKG_VERSION"), config) {
        Ok(u) => Some(u),
        Err(e) => {
            tracing::warn!(
                "self-update disabled: failed to construct updater: {e} (likely no manifest URL)"
            );
            None
        }
    }
}

#[cfg(feature = "selfupdate")]
fn handle_update(
    action: &UpdateAction,
    manifest_url: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use rs_f4ss_core::selfupdate::{SelfUpdateConfig, SelfUpdater, UpdateInfo};

    let mut config = SelfUpdateConfig::from_env();
    if let Some(url) = manifest_url {
        config.manifest_url = url.to_string();
    }

    let updater = SelfUpdater::new(env!("CARGO_PKG_VERSION"), config)?;

    match action {
        UpdateAction::Version => {
            let info = UpdateInfo::from_updater(&updater);
            println!("rs-f4ss v{}", info.current_version);
            println!("  manifest:   {}", info.manifest_url);
            println!("  platform:   {}", info.platform);
            println!(
                "  exe:        {}",
                info.exe_path.as_deref().unwrap_or("<unknown>")
            );
            println!("  integrity:  sha256 (update channel: HTTPS manifest)");
            // Surface the post-apply pending state so the operator can
            // confirm `apply` succeeded without poking the REST API.
            println!(
                "  pending:    {}",
                if info.pending_update {
                    "yes — binary on disk is newer, restart to use it"
                } else {
                    "no"
                }
            );
        }
        UpdateAction::Check => {
            eprintln!("Checking {} …", updater.manifest_url());
            match updater.check() {
                Ok(Some(release)) => {
                    let size = release
                        .asset_for_current_platform()
                        .map(|a| a.size)
                        .unwrap_or(0);
                    println!(
                        "Update available: {} -> {} ({} bytes, released {})",
                        updater.current_version(),
                        release.version,
                        size,
                        release.date
                    );
                }
                Ok(None) => {
                    println!(
                        "Already up to date (current = {}, latest = {})",
                        updater.current_version(),
                        updater.current_version()
                    );
                }
                Err(e) => return Err(format!("check failed: {e}").into()),
            }
        }
        UpdateAction::Apply { no_restart } => {
            let release = match updater.check()? {
                Some(r) => r,
                None => {
                    println!("Already up to date: {}", updater.current_version());
                    return Ok(());
                }
            };
            eprintln!(
                "Updating {} -> {} from {}",
                updater.current_version(),
                release.version,
                updater.manifest_url()
            );
            if *no_restart {
                updater.apply(&release)?;
                println!(
                    "Update {} installed. Restart {} manually to use the new version.",
                    release.version,
                    SelfUpdater::current_exe_path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| "the binary".into())
                );
            } else {
                updater.apply_and_restart(&release)?;
                // apply_and_restart() does not return on success — it execv's the
                // new binary in place. We only get here on error.
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn run_with_cli(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Some(Commands::Status) => return handle_status(),
        Some(Commands::Unmount { ref mountpoint }) => return os::handle_unmount(mountpoint),
        Some(Commands::Serve {
            ref listen,
            ref config,
            ref stop,
            ref api_user,
            ref api_pass,
        }) => {
            #[cfg(feature = "api")]
            {
                if *stop {
                    return handle_serve_stop(
                        config.as_deref(),
                        api_user.as_deref(),
                        api_pass.as_deref(),
                    );
                }
                return handle_serve(listen.clone(), config.as_deref());
            }
            #[cfg(not(feature = "api"))]
            {
                eprintln!("Serve command requires 'api' feature. Rebuild with: cargo build --features api");
                std::process::exit(1);
            }
        }
        Some(Commands::Mount {
            ref action,
            ref api_user,
            ref api_pass,
        }) => match action {
            MountAction::List { ref api } => {
                return api_list(api, api_user.as_deref(), api_pass.as_deref())
            }
            MountAction::Add {
                ref id,
                ref url,
                ref path,
                ref user,
                ref pass,
                read_only,
                cache_ttl,
                cache_size,
                ref api,
            } => {
                let mut body = serde_json::json!({
                    "id": id,
                    "url": url,
                    "mountpoint": path,
                    "read_only": *read_only,
                    "cache_ttl_secs": *cache_ttl,
                    "cache_size": *cache_size,
                });
                if let Some(u) = user.as_deref() {
                    body["username"] = serde_json::Value::String(u.to_string());
                }
                if let Some(p) = pass.as_deref() {
                    body["password"] = serde_json::Value::String(p.to_string());
                }
                return api_add(api, api_user.as_deref(), api_pass.as_deref(), id, body);
            }
            MountAction::Del { ref id, ref api } => {
                return api_del(api, api_user.as_deref(), api_pass.as_deref(), id)
            }
            MountAction::Start { ref id, ref api } => {
                return api_start(api, api_user.as_deref(), api_pass.as_deref(), id)
            }
            MountAction::Stop { ref id, ref api } => {
                return api_stop(api, api_user.as_deref(), api_pass.as_deref(), id)
            }
        },
        Some(Commands::Share {
            ref action,
            ref api_user,
            ref api_pass,
        }) => match action {
            ShareAction::Serve {
                ref path,
                ref listen,
                ref user,
                ref pass,
                read_only,
            } => {
                #[cfg(feature = "serve")]
                return handle_share(path, listen, user.as_deref(), pass.as_deref(), *read_only);
                #[cfg(not(feature = "serve"))]
                {
                    let _ = (path, listen, user, pass, read_only);
                    eprintln!("Share serve requires 'serve' feature. Rebuild with: cargo build --features serve");
                    std::process::exit(1);
                }
            }
            ShareAction::List { ref api } => {
                return api_share_list(api, api_user.as_deref(), api_pass.as_deref())
            }
            ShareAction::Add {
                ref id,
                ref path,
                ref listen,
                ref user,
                ref pass,
                read_only,
                ref api,
            } => {
                let mut body = serde_json::json!({
                    "id": id,
                    "path": path,
                    "addr": listen,
                    "read_only": *read_only,
                });
                if let Some(u) = user.as_deref() {
                    body["user"] = serde_json::Value::String(u.to_string());
                }
                if let Some(p) = pass.as_deref() {
                    body["pass"] = serde_json::Value::String(p.to_string());
                }
                return api_share_add(api, api_user.as_deref(), api_pass.as_deref(), id, body);
            }
            ShareAction::Del { ref id, ref api } => {
                return api_share_del(api, api_user.as_deref(), api_pass.as_deref(), id)
            }
            ShareAction::Start { ref id, ref api } => {
                return api_share_start(api, api_user.as_deref(), api_pass.as_deref(), id)
            }
            ShareAction::Stop { ref id, ref api } => {
                return api_share_stop(api, api_user.as_deref(), api_pass.as_deref(), id)
            }
        },
        #[cfg(feature = "selfupdate")]
        Some(Commands::Update {
            ref action,
            ref manifest_url,
        }) => return handle_update(action, manifest_url.as_deref()),
        None => {}
    }

    // ── Mount mode ──
    let url = cli
        .url
        .as_deref()
        .ok_or("Missing URL.\nUsage: rs-f4ss <url> <mountpoint>")?;
    let mountpoint_str = cli
        .mountpoint
        .as_deref()
        .ok_or("Missing mountpoint.\nUsage: rs-f4ss <url> <mountpoint>")?;
    let mountpoint = PathBuf::from(mountpoint_str);

    os::validate_mountpoint(&mountpoint)?;

    // Resolve password: --pass > --pass-file > $RS_F4SS_PASSWORD
    let password = if let Some(p) = &cli.pass {
        Some(p.clone())
    } else if let Some(path) = &cli.pass_file {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Cannot read --pass-file: {e}"))?;
        Some(content.trim_end().to_string())
    } else {
        std::env::var("RS_F4SS_PASSWORD").ok()
    };

    let backend = resolve_backend(url, cli.read_only, cli.user.as_deref(), password.as_deref())?;

    tracing::info!(
        "Backend: {} at {} (readonly={})",
        backend.protocol(),
        backend.server_addr(),
        backend.is_read_only()
    );

    // Windows: create shared unmount callback slot for Ctrl+C handler
    #[cfg(target_os = "windows")]
    let unmount_cb: Arc<std::sync::Mutex<Option<rs_f4ss_core::mount::UnmountCallback>>> =
        Arc::new(std::sync::Mutex::new(None));

    #[cfg(target_os = "windows")]
    let unmount_cb_clone = unmount_cb.clone();

    let config = MountConfig {
        mountpoint: mountpoint.clone(),
        read_only: cli.read_only,
        cache_ttl: Duration::from_secs(cli.cache_ttl),
        cache_size: cli.cache_size,
        allow_other: cli.allow_other,
        #[cfg(unix)]
        mount_uid: unsafe { libc::getuid() },
        #[cfg(unix)]
        mount_gid: unsafe { libc::getgid() },
        #[cfg(not(unix))]
        mount_uid: 0,
        #[cfg(not(unix))]
        mount_gid: 0,
        on_mount_ready: None,
        #[cfg(target_os = "windows")]
        on_set_unmount: Some(Arc::new(move |cb| {
            *unmount_cb_clone.lock().unwrap() = Some(cb);
        })),
        #[cfg(not(target_os = "windows"))]
        on_set_unmount: None,
    };

    let engine = MountEngine::new(backend, config);
    let mut events = engine.subscribe();

    // Subscribe to events for logging
    std::thread::spawn(move || {
        while let Ok(event) = events.blocking_recv() {
            match &event {
                MountEvent::MountStarted { mountpoint } => {
                    tracing::info!("Mount started at {}", mountpoint.display());
                }
                MountEvent::MountStopped => {
                    tracing::info!("Mount stopped");
                }
                MountEvent::Error { error } => {
                    tracing::error!("Error: {error}");
                }
                MountEvent::CacheHit { path } => {
                    tracing::debug!("Cache hit: {}", path.display());
                }
                MountEvent::CacheMiss { path } => {
                    tracing::debug!("Cache miss: {}", path.display());
                }
                MountEvent::FileRead { path, bytes, .. } => {
                    tracing::debug!("Read {} bytes from {}", bytes, path.display());
                }
                MountEvent::FileWritten { path, bytes, .. } => {
                    tracing::info!("Written {} bytes to {}", bytes, path.display());
                }
                MountEvent::DirListed { path, entries } => {
                    tracing::debug!("Listed {} entries in {}", entries, path.display());
                }
                MountEvent::Connected { url } => {
                    tracing::info!("Connected to {url}");
                }
            }
        }
    });

    // Set up Ctrl+C handler for graceful unmount
    #[cfg(target_os = "windows")]
    os::setup_ctrlc_handler(mountpoint.clone(), unmount_cb);
    #[cfg(not(target_os = "windows"))]
    os::setup_ctrlc_handler(mountpoint.clone());

    tracing::info!("Mounting {url} at {mountpoint_str}");
    if cli.foreground {
        tracing::info!("Press Ctrl+C to unmount.");
    }

    // mount() is synchronous — blocks until unmount
    let result = engine.mount();

    if let Err(e) = &result {
        tracing::error!("Mount failed: {e}");
    }

    result.map_err(|e| e.into())
}

// ---------------------------------------------------------------------------
// Remote API commands (list / add / del / start / stop)
// ---------------------------------------------------------------------------

fn api_base(addr: &str) -> String {
    let addr = addr.trim();
    if addr.starts_with("http://") || addr.starts_with("https://") {
        addr.trim_end_matches('/').to_string()
    } else {
        format!("http://{addr}").trim_end_matches('/').to_string()
    }
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("failed to create HTTP client")
}

fn api_get(
    addr: &str,
    path: &str,
    user: Option<&str>,
    pass: Option<&str>,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let url = format!("{}/api{path}", api_base(addr));
    let mut req = http_client().get(&url);
    if let (Some(u), Some(p)) = (user, pass) {
        req = req.basic_auth(u, Some(p));
    }
    let resp = req.send()?;
    let status = resp.status();
    let body: serde_json::Value = resp.json()?;
    if !status.is_success() {
        let msg = body["error"].as_str().unwrap_or("unknown error");
        return Err(format!("{status}: {msg}").into());
    }
    Ok(body)
}

fn api_post(
    addr: &str,
    path: &str,
    body: Option<serde_json::Value>,
    user: Option<&str>,
    pass: Option<&str>,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let url = format!("{}/api{path}", api_base(addr));
    let client = http_client();
    let resp = if let Some(b) = body {
        let mut req = client.post(&url).json(&b);
        if let (Some(u), Some(p)) = (user, pass) {
            req = req.basic_auth(u, Some(p));
        }
        req.send()?
    } else {
        let mut req = client.post(&url);
        if let (Some(u), Some(p)) = (user, pass) {
            req = req.basic_auth(u, Some(p));
        }
        req.send()?
    };
    let status = resp.status();
    let resp_body: serde_json::Value = resp.json()?;
    if !status.is_success() {
        let msg = resp_body["error"].as_str().unwrap_or("unknown error");
        return Err(format!("{status}: {msg}").into());
    }
    Ok(resp_body)
}

fn api_delete(
    addr: &str,
    path: &str,
    user: Option<&str>,
    pass: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!("{}/api{path}", api_base(addr));
    let mut req = http_client().delete(&url);
    if let (Some(u), Some(p)) = (user, pass) {
        req = req.basic_auth(u, Some(p));
    }
    let resp = req.send()?;
    let status = resp.status();
    if !status.is_success() && status.as_u16() != 204 {
        let body: serde_json::Value = resp.json().unwrap_or_default();
        let msg = body["error"].as_str().unwrap_or("unknown error");
        return Err(format!("{status}: {msg}").into());
    }
    Ok(())
}

fn api_list(
    addr: &str,
    user: Option<&str>,
    pass: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mounts = api_get(addr, "/mounts", user, pass)?;
    let list = mounts.as_array().ok_or("Invalid response")?;
    if list.is_empty() {
        println!("No mounts configured.");
        return Ok(());
    }
    println!(
        "{:<12} {:<8} {:<30} {:<30} RO",
        "ID", "STATE", "URL", "MOUNTPOINT"
    );
    for m in list {
        let id = m["id"].as_str().unwrap_or("-");
        let state = m["state"].as_str().unwrap_or("-");
        let url = m["url"].as_str().unwrap_or("-");
        let mp = m["mountpoint"].as_str().unwrap_or("-");
        let ro = if m["read_only"].as_bool().unwrap_or(false) {
            "ro"
        } else {
            "rw"
        };
        println!("{:<12} {:<8} {:<30} {:<30} {}", id, state, url, mp, ro);
    }
    Ok(())
}

fn api_add(
    addr: &str,
    user: Option<&str>,
    pass: Option<&str>,
    id: &str,
    body: serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    api_post(addr, "/mounts", Some(body), user, pass)?;
    println!("Added mount: {id}");
    Ok(())
}

fn api_del(
    addr: &str,
    user: Option<&str>,
    pass: Option<&str>,
    id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = api_post(addr, &format!("/mounts/{id}/stop"), None, user, pass);
    api_delete(addr, &format!("/mounts/{id}"), user, pass)?;
    println!("Deleted mount: {id}");
    Ok(())
}

fn api_start(
    addr: &str,
    user: Option<&str>,
    pass: Option<&str>,
    id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    api_post(addr, &format!("/mounts/{id}/start"), None, user, pass)?;
    println!("Starting mount: {id}");
    Ok(())
}

fn api_stop(
    addr: &str,
    user: Option<&str>,
    pass: Option<&str>,
    id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    api_post(addr, &format!("/mounts/{id}/stop"), None, user, pass)?;
    println!("Stopped mount: {id}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Serve mode (REST API)
// ---------------------------------------------------------------------------

#[cfg(feature = "api")]
fn handle_serve(
    listen: Option<String>,
    config_path: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let _lock = os::try_acquire_serve_lock()?;

    let path = match config_path {
        Some(p) => std::path::PathBuf::from(p),
        None => rs_f4ss_core::persistence::default_config_path()
            .ok_or("Cannot determine config directory")?,
    };

    let store_listen = rs_f4ss_core::persistence::load_listen(&path);
    let listen = resolve_listen(listen.as_deref(), store_listen.as_deref());

    let auth = rs_f4ss_core::persistence::load_auth(&path);
    tracing::info!("Auth user: {}", auth.username);
    let default_creds = rs_f4ss_core::persistence::is_default_auth(&auth);
    if default_creds {
        tracing::warn!("Using default credentials (admin:admin). Please change the password via Web UI or CLI.");
        if !is_loopback_addr(&listen) {
            return Err(format!(
                "Refusing to serve on {listen} with default credentials (admin:admin). \
                 Change the password first (Web UI or `rs-f4ss serve` on 127.0.0.1), \
                 or bind to a loopback address, e.g. --listen 127.0.0.1:8080."
            )
            .into());
        }
    }

    let state = std::sync::Arc::new(rs_f4ss_core::api::AppState {
        mounts: {
            let m = rs_f4ss_core::MountManager::new_with_persistence(path.clone());
            m.restore_entries();
            m
        },
        #[cfg(feature = "serve")]
        shares: {
            let s = rs_f4ss_core::ShareManager::new_with_persistence(path.clone());
            s.restore_entries();
            s
        },
        auth: std::sync::Mutex::new(auth),
        persist_path: path,
        #[cfg(feature = "selfupdate")]
        updater: build_default_updater(),
    });
    let app = rs_f4ss_core::api::create_router(state);

    tracing::info!("REST API listening on {listen}");
    tracing::info!("Endpoints: GET /api/health, GET /api/mounts, GET /api/shares, ...");

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(listen.as_str()).await?;
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                rs_f4ss_core::api::shutdown_signal().await;
                tracing::info!("Shutdown requested via API");
            })
            .await
    })?;

    Ok(())
}

/// Serve listen address precedence: `--listen` flag > config file `listen`
/// key > built-in default.
#[cfg(feature = "api")]
fn resolve_listen(cli: Option<&str>, store: Option<&str>) -> String {
    cli.map(str::to_string)
        .or_else(|| store.map(str::to_string))
        .unwrap_or_else(|| "0.0.0.0:8080".to_string())
}

/// Build the loopback URL of the shutdown endpoint from the serve listen
/// address. The bind address is not necessarily reachable from this
/// machine (0.0.0.0, LAN IPs), so the host is always replaced with
/// 127.0.0.1 and only the port is kept.
#[cfg(feature = "api")]
fn resolve_shutdown_url(store_listen: Option<&str>) -> String {
    let listen = store_listen.unwrap_or("0.0.0.0:8080");
    let port = listen.rsplit_once(':').map(|(_, p)| p).unwrap_or("8080");
    format!("http://127.0.0.1:{port}/api/shutdown")
}

/// `serve --stop`: gracefully stop the background serve instance by
/// calling its authenticated `POST /api/shutdown` endpoint. The PID file
/// decides whether an instance is running at all (stale PID files are
/// cleaned up); the listen address comes from the config file.
#[cfg(feature = "api")]
fn handle_serve_stop(
    config_path: Option<&str>,
    api_user: Option<&str>,
    api_pass: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let pid_path = os::serve_pid_path().ok_or("Cannot determine serve PID file path")?;
    let pid: Option<u32> = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|content| content.trim().parse().ok());
    let Some(pid) = pid else {
        println!("No serve instance running");
        return Ok(());
    };
    if !os::is_pid_alive(pid) {
        // Stale PID file from a dead serve — clean it up (idempotent stop).
        let _ = std::fs::remove_file(&pid_path);
        println!("No serve instance running");
        return Ok(());
    }

    let (user, pass) = match (api_user, api_pass) {
        (Some(u), Some(p)) => (u, p),
        _ => {
            return Err(
                "serve --stop requires API credentials: pass --api-user/--api-pass \
                 or set RS_F4SS_API_USER/RS_F4SS_API_PASS"
                    .into(),
            )
        }
    };

    let config_file = match config_path {
        Some(p) => std::path::PathBuf::from(p),
        None => rs_f4ss_core::persistence::default_config_path()
            .ok_or("Cannot determine config directory")?,
    };
    let store_listen = rs_f4ss_core::persistence::load_listen(&config_file);
    let url = resolve_shutdown_url(store_listen.as_deref());

    let client = reqwest::blocking::ClientBuilder::new()
        .timeout(Duration::from_secs(5))
        .build()?;
    let resp = client
        .post(&url)
        .basic_auth(user, Some(pass))
        .send()
        .map_err(|e| {
            format!(
                "Cannot reach serve API at {url} — is it running? ({e}) \
             If the process is stuck, stop it manually (e.g. Stop-Process -Id {pid})."
            )
        })?;
    let status = resp.status();
    if status.as_u16() == 401 {
        return Err("Unauthorized (401): wrong API credentials".into());
    }
    if !status.is_success() {
        return Err(format!("serve API returned {status}").into());
    }
    println!("Serve (PID {pid}) shutting down");
    Ok(())
}

fn main() {
    // Parse CLI args before doing anything (needed for daemonize decision)
    let cli = Cli::parse();

    // Daemonize (fork) before initializing tracing, so child gets fresh state
    #[cfg(target_os = "linux")]
    if cli.command.is_none() && !cli.foreground {
        if let Some(ref mp) = cli.mountpoint {
            let mp = PathBuf::from(mp);
            if mp.is_dir() {
                if let Err(e) = os::daemonize(&mp) {
                    eprintln!("Daemonize failed: {e}");
                    std::process::exit(1);
                }
            }
        }
    }

    // Now safe to init tracing (child process's first init, or parent already exited)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let os_name = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "unknown"
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    };
    tracing::info!(
        "rs-f4ss v{}-{} ({}-{})",
        env!("CARGO_PKG_VERSION"),
        env!("GIT_HASH"),
        os_name,
        arch,
    );

    if let Err(e) = run_with_cli(cli) {
        tracing::error!("Error: {e}");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// Share mode (HTTP + WebDAV file server)
// ---------------------------------------------------------------------------

/// Whether `addr` binds to this machine only.
#[cfg(any(feature = "api", feature = "serve"))]
fn is_loopback_addr(addr: &str) -> bool {
    use std::net::{IpAddr, Ipv6Addr};

    let host = if let Some(rest) = addr.strip_prefix('[') {
        // Bracketed IPv6, e.g. "[::1]:8080" or "[::]".
        match rest.split_once(']') {
            Some((h, _)) => h,
            None => return false,
        }
    } else if let Ok(v6) = addr.parse::<Ipv6Addr>() {
        // Bare IPv6 without brackets, e.g. "::1".
        return v6.is_loopback();
    } else {
        // "host:port"; the host is empty for ":8080", which binds every interface.
        match addr.rsplit_once(':') {
            Some((h, _)) => h,
            None => addr,
        }
    };

    if host.is_empty() {
        return false;
    }
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Whether to warn about an unauthenticated server reachable from the network.
#[cfg(feature = "serve")]
fn needs_no_auth_warning(addr: &str, has_auth: bool) -> bool {
    !has_auth && !is_loopback_addr(addr)
}

/// Resolve `--user`/`--pass` into an optional `(username, password_hash)` pair.
#[cfg(feature = "serve")]
fn parse_share_auth(
    user: Option<&str>,
    pass: Option<&str>,
) -> Result<Option<(String, String)>, String> {
    match (user, pass) {
        (Some(u), Some(p)) => Ok(Some((
            u.to_string(),
            rs_f4ss_core::persistence::sha256_hex(p),
        ))),
        (Some(_), None) | (None, Some(_)) => {
            Err("Both --user and --pass are required for authentication".to_string())
        }
        (None, None) => Ok(None),
    }
}

#[cfg(feature = "serve")]
fn handle_share(
    path: &str,
    listen: &str,
    user: Option<&str>,
    pass: Option<&str>,
    read_only: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(path);
    if !root.is_dir() {
        return Err(format!("Not a directory: {path}").into());
    }

    let auth = parse_share_auth(user, pass)?;
    let has_auth = auth.is_some();

    let config = rs_f4ss_core::server::FileServerConfig {
        root,
        read_only,
        auth,
    };

    tracing::info!("Sharing {} at {listen} (readonly={})", path, read_only);
    if needs_no_auth_warning(listen, has_auth) {
        let access = if read_only {
            "readable by anyone"
        } else {
            "readable and writable by anyone"
        };
        tracing::warn!(
            "Serving {path} on {listen} with NO authentication — the directory is {access} who can reach this address. Bind to a loopback address (the default) or pass --user/--pass."
        );
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(rs_f4ss_core::server::serve(config, listen))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Share API commands
// ---------------------------------------------------------------------------

fn api_share_list(
    addr: &str,
    user: Option<&str>,
    pass: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let shares = api_get(addr, "/shares", user, pass)?;
    let list = shares.as_array().ok_or("Invalid response")?;
    if list.is_empty() {
        println!("No shares configured.");
        return Ok(());
    }
    println!(
        "{:<12} {:<8} {:<30} {:<25} RO",
        "ID", "STATE", "PATH", "ADDR"
    );
    for s in list {
        let id = s["id"].as_str().unwrap_or("-");
        let state = s["state"].as_str().unwrap_or("-");
        let path = s["path"].as_str().unwrap_or("-");
        let saddr = s["addr"].as_str().unwrap_or("-");
        let ro = if s["read_only"].as_bool().unwrap_or(false) {
            "ro"
        } else {
            "rw"
        };
        println!("{:<12} {:<8} {:<30} {:<25} {}", id, state, path, saddr, ro);
    }
    Ok(())
}

fn api_share_add(
    addr: &str,
    user: Option<&str>,
    pass: Option<&str>,
    id: &str,
    body: serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    api_post(addr, "/shares", Some(body), user, pass)?;
    println!("Added share: {id}");
    Ok(())
}

fn api_share_del(
    addr: &str,
    user: Option<&str>,
    pass: Option<&str>,
    id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = api_post(addr, &format!("/shares/{id}/stop"), None, user, pass);
    api_delete(addr, &format!("/shares/{id}"), user, pass)?;
    println!("Deleted share: {id}");
    Ok(())
}

fn api_share_start(
    addr: &str,
    user: Option<&str>,
    pass: Option<&str>,
    id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    api_post(addr, &format!("/shares/{id}/start"), None, user, pass)?;
    println!("Starting share: {id}");
    Ok(())
}

fn api_share_stop(
    addr: &str,
    user: Option<&str>,
    pass: Option<&str>,
    id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    api_post(addr, &format!("/shares/{id}/stop"), None, user, pass)?;
    println!("Stopped share: {id}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse_cli(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    #[test]
    fn test_parse_basic() {
        let cli = parse_cli(&["rs-f4ss", "http://host:5000", "/mnt"]).unwrap();
        assert_eq!(cli.url.as_deref(), Some("http://host:5000"));
        assert_eq!(cli.mountpoint.as_deref(), Some("/mnt"));
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_parse_auth() {
        let cli = parse_cli(&[
            "rs-f4ss",
            "http://host:5000",
            "/mnt",
            "--user",
            "a",
            "--pass",
            "b",
        ])
        .unwrap();
        assert_eq!(cli.user.as_deref(), Some("a"));
        assert_eq!(cli.pass.as_deref(), Some("b"));
    }

    #[test]
    fn test_parse_readonly() {
        let cli = parse_cli(&["rs-f4ss", "http://host:5000", "/mnt", "--read-only"]).unwrap();
        assert!(cli.read_only);
    }

    #[test]
    fn test_parse_cache_ttl() {
        let cli = parse_cli(&["rs-f4ss", "http://host:5000", "/mnt", "--cache-ttl", "30"]).unwrap();
        assert_eq!(cli.cache_ttl, 30);
    }

    #[test]
    fn test_parse_no_args_is_ok_no_command() {
        let cli = parse_cli(&["rs-f4ss"]).unwrap();
        assert!(cli.command.is_none());
        assert!(cli.url.is_none());
        assert!(cli.mountpoint.is_none());
    }

    #[test]
    fn test_parse_status() {
        let cli = parse_cli(&["rs-f4ss", "status"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Status)));
    }

    #[test]
    fn test_parse_unmount() {
        let cli = parse_cli(&["rs-f4ss", "unmount", "/mnt/dufs"]).unwrap();
        match cli.command {
            Some(Commands::Unmount { mountpoint }) => assert_eq!(mountpoint, "/mnt/dufs"),
            _ => panic!("Expected Unmount subcommand"),
        }
    }

    #[cfg(feature = "webdav")]
    #[test]
    fn test_resolve_http() {
        let backend = resolve_backend("http://host:5000", false, None, None).unwrap();
        assert_eq!(backend.protocol(), "webdav");
    }

    #[cfg(feature = "webdav")]
    #[test]
    fn test_resolve_https() {
        let backend = resolve_backend("https://host:5000", false, None, None).unwrap();
        assert_eq!(backend.protocol(), "webdav");
    }

    #[cfg(feature = "webdav")]
    #[test]
    fn test_resolve_webdav_scheme() {
        let backend = resolve_backend("webdav://host", false, None, None).unwrap();
        assert_eq!(backend.protocol(), "webdav");
    }

    #[test]
    fn test_resolve_unsupported() {
        let result = resolve_backend("ftp://host", false, None, None);
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("Unsupported"));
    }

    #[test]
    fn test_resolve_no_scheme() {
        let result = resolve_backend("host:5000", false, None, None);
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("Invalid URL"));
    }

    #[cfg(feature = "webdav")]
    #[test]
    fn test_resolve_webdavs_scheme() {
        let backend = resolve_backend("webdavs://host", false, None, None).unwrap();
        assert_eq!(backend.protocol(), "webdav");
    }

    #[cfg(feature = "webdav")]
    #[test]
    fn test_resolve_auth_partial() {
        let result = resolve_backend("http://host", false, Some("user"), None);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_pass_file_arg() {
        let cli = parse_cli(&[
            "rs-f4ss",
            "http://host:5000",
            "/mnt",
            "--pass-file",
            "/tmp/secret",
        ])
        .unwrap();
        assert_eq!(cli.pass_file.as_deref(), Some("/tmp/secret"));
    }

    #[test]
    fn test_password_priority_pass_over_file() {
        let cli = parse_cli(&[
            "rs-f4ss",
            "http://host:5000",
            "/mnt",
            "--pass",
            "secret",
            "--pass-file",
            "/tmp/secret",
        ])
        .unwrap();
        assert_eq!(cli.pass.as_deref(), Some("secret"));
        assert_eq!(cli.pass_file.as_deref(), Some("/tmp/secret"));
    }

    #[test]
    fn test_parse_serve() {
        let cli = parse_cli(&["rs-f4ss", "serve", "--listen", "0.0.0.0:9999"]).unwrap();
        match cli.command {
            Some(Commands::Serve { ref listen, .. }) => {
                assert_eq!(listen.as_deref(), Some("0.0.0.0:9999"))
            }
            _ => panic!("Expected Serve"),
        }
    }

    #[test]
    fn test_parse_mount_list() {
        let cli = parse_cli(&["rs-f4ss", "mount", "list"]).unwrap();
        match cli.command {
            Some(Commands::Mount {
                action: MountAction::List { .. },
                ..
            }) => {}
            _ => panic!("Expected Mount List"),
        }
    }

    #[test]
    fn test_parse_mount_add() {
        let cli = parse_cli(&[
            "rs-f4ss",
            "mount",
            "add",
            "myserver",
            "--url",
            "http://host",
            "--path",
            "/mnt",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Mount {
                action:
                    MountAction::Add {
                        ref id,
                        ref url,
                        ref path,
                        ..
                    },
                ..
            }) => {
                assert_eq!(id, "myserver");
                assert_eq!(url, "http://host");
                assert_eq!(path, "/mnt");
            }
            _ => panic!("Expected Mount Add"),
        }
    }

    #[test]
    fn test_parse_share_serve() {
        let cli = parse_cli(&["rs-f4ss", "share", "serve", "/data", "--listen", ":9090"]).unwrap();
        match cli.command {
            Some(Commands::Share {
                action:
                    ShareAction::Serve {
                        ref path,
                        ref listen,
                        ..
                    },
                ..
            }) => {
                assert_eq!(path, "/data");
                assert_eq!(listen, ":9090");
            }
            _ => panic!("Expected Share Serve"),
        }
    }

    #[test]
    fn test_parse_share_list() {
        let cli = parse_cli(&["rs-f4ss", "share", "list"]).unwrap();
        match cli.command {
            Some(Commands::Share {
                action: ShareAction::List { .. },
                ..
            }) => {}
            _ => panic!("Expected Share List"),
        }
    }

    #[cfg(feature = "serve")]
    #[test]
    fn test_share_serve_default_listen_is_loopback() {
        let cli = parse_cli(&["rs-f4ss", "share", "serve", "/data"]).unwrap();
        match cli.command {
            Some(Commands::Share {
                action: ShareAction::Serve { ref listen, .. },
                ..
            }) => {
                assert_eq!(listen, "127.0.0.1:8080");
                assert!(is_loopback_addr(listen));
            }
            _ => panic!("Expected Share Serve"),
        }
    }

    #[cfg(feature = "serve")]
    #[test]
    fn test_is_loopback_addr_truth_table() {
        for addr in ["127.0.0.1:8080", "localhost:8080", "[::1]:8080", "::1"] {
            assert!(is_loopback_addr(addr), "{addr} should be loopback");
        }
        for addr in [
            "0.0.0.0:8080",
            "192.168.1.5:9000",
            "[::]:8080",
            "10.0.0.1:80",
        ] {
            assert!(!is_loopback_addr(addr), "{addr} should NOT be loopback");
        }
    }

    #[cfg(feature = "serve")]
    #[test]
    fn test_parse_share_auth_pass_only_is_error() {
        let result = parse_share_auth(None, Some("p"));
        assert!(result.is_err(), "pass without user must be an error");
    }

    #[cfg(feature = "serve")]
    #[test]
    fn test_parse_share_auth_user_only_is_error() {
        let result = parse_share_auth(Some("u"), None);
        assert!(result.is_err(), "user without pass must be an error");
    }

    #[cfg(feature = "serve")]
    #[test]
    fn test_parse_share_auth_both_some_enables_auth() {
        let auth = parse_share_auth(Some("u"), Some("p")).unwrap();
        let (user, hash) = auth.expect("auth should be enabled");
        assert_eq!(user, "u");
        assert_eq!(hash, rs_f4ss_core::persistence::sha256_hex("p"));
    }

    #[cfg(feature = "api")]
    #[test]
    fn test_resolve_listen_cli_wins() {
        assert_eq!(
            resolve_listen(Some("127.0.0.1:1"), Some("10.0.0.9:2")),
            "127.0.0.1:1"
        );
    }

    #[cfg(feature = "api")]
    #[test]
    fn test_resolve_listen_config_file_fallback() {
        assert_eq!(
            resolve_listen(None, Some("127.0.0.1:9999")),
            "127.0.0.1:9999"
        );
    }

    #[cfg(feature = "api")]
    #[test]
    fn test_resolve_listen_defaults_when_both_missing() {
        assert_eq!(resolve_listen(None, None), "0.0.0.0:8080");
    }

    #[cfg(feature = "api")]
    #[test]
    fn test_resolve_shutdown_url_replaces_wildcard_host() {
        assert_eq!(
            resolve_shutdown_url(Some("0.0.0.0:8081")),
            "http://127.0.0.1:8081/api/shutdown"
        );
    }

    #[cfg(feature = "api")]
    #[test]
    fn test_resolve_shutdown_url_defaults_when_no_listen() {
        assert_eq!(
            resolve_shutdown_url(None),
            "http://127.0.0.1:8080/api/shutdown"
        );
    }

    #[cfg(feature = "api")]
    #[test]
    fn test_resolve_shutdown_url_keeps_port_for_specific_host() {
        assert_eq!(
            resolve_shutdown_url(Some("192.168.1.5:9000")),
            "http://127.0.0.1:9000/api/shutdown"
        );
    }

    #[test]
    fn test_parse_serve_stop() {
        let cli = parse_cli(&["rs-f4ss", "serve", "--stop"]).unwrap();
        match cli.command {
            Some(Commands::Serve { stop: true, .. }) => {}
            _ => panic!("Expected Serve --stop"),
        }
    }

    #[test]
    fn test_is_pid_alive_current_process() {
        assert!(os::is_pid_alive(std::process::id()));
    }

    #[test]
    fn test_is_pid_alive_nonexistent_pid() {
        assert!(!os::is_pid_alive(4_000_000));
    }

    #[test]
    fn test_serve_pid_path_is_serve_pid() {
        let path = os::serve_pid_path().expect("serve PID path should be resolvable");
        assert_eq!(path.file_name().unwrap().to_string_lossy(), "serve.pid");
    }

    #[cfg(feature = "serve")]
    #[test]
    fn test_parse_share_auth_none_none_is_disabled() {
        let auth = parse_share_auth(None, None).unwrap();
        assert!(auth.is_none(), "no credentials means auth disabled");
    }

    #[cfg(feature = "serve")]
    #[test]
    fn test_needs_no_auth_warning() {
        assert!(needs_no_auth_warning("0.0.0.0:8080", false));
        assert!(needs_no_auth_warning("192.168.1.5:9000", false));
        assert!(!needs_no_auth_warning("127.0.0.1:8080", false));
        assert!(!needs_no_auth_warning("0.0.0.0:8080", true));
    }
}
