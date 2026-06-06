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
        #[arg(long, default_value = "0.0.0.0:8080", help = "Listen address")]
        listen: String,
        #[arg(long, help = "Config file path (default: platform config dir)")]
        config: Option<String>,
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
        #[arg(long, default_value = "0.0.0.0:8080", help = "Listen address")]
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
// Main
// ---------------------------------------------------------------------------

fn run_with_cli(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Some(Commands::Status) => return handle_status(),
        Some(Commands::Unmount { ref mountpoint }) => return os::handle_unmount(mountpoint),
        Some(Commands::Serve {
            ref listen,
            ref config,
        }) => {
            #[cfg(feature = "api")]
            return handle_serve(listen, config.as_deref());
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
fn handle_serve(listen: &str, config_path: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let _lock = os::try_acquire_serve_lock()?;

    let path = match config_path {
        Some(p) => std::path::PathBuf::from(p),
        None => rs_f4ss_core::persistence::default_config_path()
            .ok_or("Cannot determine config directory")?,
    };

    let auth = rs_f4ss_core::persistence::load_auth(&path);
    tracing::info!("Auth user: {}", auth.username);
    if auth.username == "admin"
        && auth.password_hash == rs_f4ss_core::persistence::sha256_hex("admin")
    {
        tracing::warn!("Using default credentials (admin:admin). Please change the password via Web UI or CLI.");
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
    });
    let app = rs_f4ss_core::api::create_router(state);

    tracing::info!("REST API listening on {listen}");
    tracing::info!("Endpoints: GET /api/health, GET /api/mounts, GET /api/shares, ...");

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(listen).await?;
        axum::serve(listener, app).await
    })?;

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

    let auth = match (user, pass) {
        (Some(u), Some(p)) => Some((u.to_string(), rs_f4ss_core::persistence::sha256_hex(p))),
        (Some(_), None) => {
            return Err("Both --user and --pass are required for authentication".into())
        }
        _ => None,
    };

    let config = rs_f4ss_core::server::FileServerConfig {
        root,
        read_only,
        auth,
    };

    tracing::info!("Sharing {} at {listen} (readonly={})", path, read_only);

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
            Some(Commands::Serve { ref listen, .. }) => assert_eq!(listen, "0.0.0.0:9999"),
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
}
