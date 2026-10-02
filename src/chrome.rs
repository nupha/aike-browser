// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Chrome process management ?find, spawn, stop.

use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::{process::Child, time::sleep};

/// Per-launch sequence number, makes auto-port user-data-dirs unique.
static LAUNCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// Child handles of live `Browser.spawn()` processes, killed at runtime exit.
/// Dropping / GC-collecting the JS handle does NOT kill the process ?that is
/// the whole point of the explicit-ownership model (`kill()` or exit cleanup).
static SPAWNED_BROWSERS: OnceLock<Mutex<Vec<Arc<tokio::sync::Mutex<Option<Child>>>>>> =
    OnceLock::new();

/// Kill every registered `Browser.spawn()` child. Called from the engine's
/// atexit / signal-handler cleanup alongside `spawn::kill_all_children`.
pub(crate) fn kill_all_spawned_browsers() {
    if let Some(reg) = SPAWNED_BROWSERS.get()
        && let Ok(guard) = reg.lock()
    {
        for arc in guard.iter() {
            if let Ok(mut slot) = arc.try_lock()
                && let Some(child) = slot.as_mut()
            {
                let _ = child.start_kill();
            }
        }
    }
}

pub(crate) fn register_spawned_child(handle: &Arc<tokio::sync::Mutex<Option<Child>>>) {
    let reg = SPAWNED_BROWSERS.get_or_init(|| Mutex::new(Vec::new()));
    reg.lock().unwrap().push(handle.clone());
}

/// Terminate a spawned browser child and **reap** it: `start_kill()` only
/// requests termination (on Windows `TerminateProcess`), and a process that is
/// never waited on stays a zombie holding its profile dir open.
///
/// `timeout` bounds the wait so a wedged child cannot hang the caller; on
/// timeout the reaper is detached and the `kill_on_drop` backstop still applies.
pub(crate) async fn kill_and_reap(child: &mut Child, timeout: Duration) {
    let _ = child.start_kill();
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            // Still alive after `timeout`: hand it to the global spawn registry
            // so runtime-exit cleanup reaps it, and drop the backstop's grip by
            // leaving the caller to hold it (kill_on_drop still applies).
            log::warn!("browser process did not exit within {timeout:?}");
        }
    }
}

/// Best-effort recursive removal of an auto-created `aike-chrome-*` profile
/// dir. Chrome holds locks until it is fully reaped, so callers do this *after*
/// [`kill_and_reap`]; a dir that is still locked is left for the next run.
fn remove_profile_dir(dir: &std::path::Path) {
    if !dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("aike-chrome-"))
    {
        return; // never delete anything we did not create
    }
    if let Err(e) = std::fs::remove_dir_all(dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::debug!("could not remove chrome profile dir {}: {e}", dir.display());
    }
}

/// A spawned browser CDP server: resolved binary, process handle, and the
/// ready browser-level CDP WebSocket URL.
pub(crate) struct SpawnedBrowser {
    pub(crate) bin: String,
    pub(crate) child: Child,
    pub(crate) cdp_url: String,
    /// Auto-created profile dir, removed after the child is reaped.
    pub(crate) user_data_dir: Option<std::path::PathBuf>,
}

/// Spawn a browser CDP server process and wait until its endpoint is ready.
///
/// - `args == None` ?auto mode: assemble the standard Chrome command line
///   (`headless`/`proxy_url` apply); `bin` defaults to auto-detected Chrome.
///   `port == 0` picks a free port discovered via DevToolsActivePort.
/// - `args == Some` ?custom command line (e.g. lightpanda): `bin` required,
///   `port` must be explicit and the endpoint is polled at that port.
pub(crate) async fn spawn(
    bin: Option<&str>,
    args: Option<&[String]>,
    headless: bool,
    port: u16,
    proxy_url: Option<&str>,
    kill_on_drop: bool,
) -> Result<SpawnedBrowser, String> {
    let user_data_dir = auto_data_dir(port);
    // Only auto mode (no explicit `args`) creates this dir; a custom command
    // line owns wherever it points, so it must never be deleted by us.
    let owns_data_dir = args.is_none();
    let (bin, args) = match (bin, args) {
        (Some(bin), Some(args)) => {
            if port == 0 {
                return Err("`args` requires an explicit `port`".to_string());
            }
            (bin.to_string(), args.to_vec())
        }
        (bin, None) => {
            let bin = bin
                .map(|s| s.to_string())
                .or_else(find_chrome)
                .ok_or_else(|| {
                    "Chrome/Chromium not found. Install Chrome or set `bin`.".to_string()
                })?;
            (bin, chrome_args(headless, port, proxy_url, &user_data_dir))
        }
        (None, Some(_)) => return Err("`args` requires `bin`".to_string()),
    };

    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if kill_on_drop {
        cmd.kill_on_drop(true);
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn `{bin}`: {e}"))?;

    // Auto port: discover the real endpoint from DevToolsActivePort; explicit
    // port: poll the known /json/version endpoint.
    let mut child = child;
    let cdp_url = match if port == 0 {
        wait_for_devtools_port(&user_data_dir, &mut child).await
    } else {
        wait_for_debug_url(port, &mut child).await
    } {
        Ok(url) => url,
        Err(e) => {
            // No caller received a handle, so the process and its profile dir
            // are ours to clean up here — otherwise they are simply orphaned.
            kill_and_reap(&mut child, Duration::from_secs(10)).await;
            if owns_data_dir {
                remove_profile_dir(&user_data_dir);
            }
            return Err(e);
        }
    };

    Ok(SpawnedBrowser {
        bin,
        child,
        cdp_url,
        user_data_dir: owns_data_dir.then_some(user_data_dir),
    })
}

/// `Browser.launch()` sugar: auto-mode spawn owning the process (killed when
/// the owning Browser object is dropped / closed).
pub(crate) async fn launch(
    headless: bool,
    port: u16,
    proxy_url: Option<&str>,
    chrome_path: Option<&str>,
) -> Result<(Child, String, Option<std::path::PathBuf>), String> {
    let spawned = spawn(chrome_path, None, headless, port, proxy_url, true).await?;
    Ok((spawned.child, spawned.cdp_url, spawned.user_data_dir))
}

/// [`kill_and_reap`] plus profile-dir cleanup, for an explicit `close()`.
///
/// The dir can only be removed once the process is fully reaped (Chrome keeps
/// its locks until then), so the removal runs after the wait.
pub(crate) async fn shutdown_and_cleanup(
    child: &mut Child,
    user_data_dir: &std::path::Path,
    timeout: Duration,
) {
    kill_and_reap(child, timeout).await;
    remove_profile_dir(user_data_dir);
}

/// Unique per-launch user-data-dir (pid + seq); explicit-port launches keep
/// the historical `aike-chrome-<port>` name.
fn auto_data_dir(port: u16) -> std::path::PathBuf {
    let dir_name = if port == 0 {
        format!(
            "aike-chrome-{}-{}",
            std::process::id(),
            LAUNCH_SEQ.fetch_add(1, Ordering::Relaxed)
        )
    } else {
        format!("aike-chrome-{}", port)
    };
    std::env::temp_dir().join(dir_name)
}

/// Assemble the standard Chrome command line (incl. anti-detection flags).
fn chrome_args(
    headless: bool,
    port: u16,
    proxy_url: Option<&str>,
    user_data_dir: &std::path::Path,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        format!("--remote-debugging-port={}", port),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-sync".to_string(),
        "--disable-background-networking".to_string(),
        "--window-size=1280,800".to_string(),
        // Use a temp user-data-dir so remote debugging port reliably opens.
        // Without this, modern Chrome on macOS may silently skip the debug.
        // Set window-size so the actual Chrome frame is large enough.
        format!("--user-data-dir={}", user_data_dir.display()),

        // === Anti-detection parameters (Phase 1) ===
        // Hide navigator.webdriver attribute (most important one)
        "--disable-blink-features=AutomationControlled".to_string(),
        // Disable automation infobar
        "--disable-automation".to_string(),
        // Disable various background features to reduce fingerprint
        "--disable-component-update".to_string(),
        "--disable-default-apps".to_string(),
        "--disable-notifications".to_string(),
        "--disable-popup-blocking".to_string(),
        "--disable-extensions".to_string(),
        "--disable-domain-reliability".to_string(),
        "--disable-background-timer-throttling".to_string(),
        "--disable-backgrounding-occluded-windows".to_string(),
        "--disable-features=ChromeWhatsNewUI,InterestFeedLoading,MediaRouter,NavigationThreading,OptimizationHints".to_string(),
        "--disable-features=SafeBrowsingEnhancedProtection".to_string(),
        "--disable-webrtc".to_string(),
        "--lang=en-US".to_string(),
        "--disable-dev-shm-usage".to_string(),
    ];

    if headless {
        args.push("--headless=new".to_string());
        // Only disable sandbox in headless mode (security risk otherwise)
        args.push("--no-sandbox".to_string());
        // Reduce crashes in some headless environments
        args.push("--disable-gpu".to_string());
    }
    if let Some(proxy) = proxy_url {
        args.push(format!("--proxy-server={}", proxy));
    }
    args
}

/// Poll `<user-data-dir>/DevToolsActivePort` (written by Chrome once the debug
/// server binds) and build the browser WS url from it: line 1 is the port,
/// line 2 is the browser target path.
///
/// The file is written slightly *before* the HTTP listener accepts connections,
/// and a Chrome that aborts during startup may leave the file behind while
/// never listening at all. So the port is probed before returning, and a child
/// that has already exited fails fast with its exit status instead of being
/// retried until the timeout.
async fn wait_for_devtools_port(
    user_data_dir: &std::path::Path,
    child: &mut Child,
) -> Result<String, String> {
    let file = user_data_dir.join("DevToolsActivePort");
    for _ in 0..300 {
        if let Ok(content) = std::fs::read_to_string(&file) {
            let mut lines = content.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next())
                && let Ok(port) = port.trim().parse::<u16>()
                && port_is_listening(port).await
            {
                return Ok(format!("ws://127.0.0.1:{}{}", port, path.trim()));
            }
        }

        // A browser that exited cannot ever publish an endpoint: report why
        // rather than burning the whole timeout on a dead process.
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "browser exited during startup ({status}) before its CDP endpoint became reachable"
            ));
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err("Chrome did not write DevToolsActivePort within 30 seconds".into())
}

/// True once `127.0.0.1:port` accepts a TCP connection, i.e. Chrome's DevTools
/// HTTP listener is actually up (the port file alone does not prove that).
async fn port_is_listening(port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_millis(500),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await,
        Ok(Ok(_))
    )
}

/// Poll Chrome's /json/version until webSocketDebuggerUrl appears.
///
/// A refused connection here is expected while the listener finishes binding,
/// so only "nothing listening yet" errors are retried; anything else (bad
/// response, malformed JSON) fails immediately with its cause.
async fn wait_for_debug_url(port: u16, child: &mut Child) -> Result<String, String> {
    let url = format!("http://127.0.0.1:{}/json/version", port);
    let client = reqwest::Client::new();
    let mut last_err = String::new();

    for _ in 0..60 {
        match client
            .get(&url)
            .timeout(Duration::from_secs(1))
            .send()
            .await
        {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    if let Some(ws_url) = json.get("webSocketDebuggerUrl").and_then(|v| v.as_str())
                    {
                        return Ok(ws_url.to_string());
                    }
                    last_err = format!("no webSocketDebuggerUrl in response from {url}");
                }
                Err(e) => last_err = format!("invalid /json/version response from {url}: {e}"),
            },
            // Not listening yet: keep polling.
            Err(e) if is_connection_refused(&e.to_string()) => {}
            Err(e) => last_err = format!("failed to query CDP endpoint {url}: {e}"),
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "browser exited during startup ({status}) before its CDP endpoint became reachable"
            ));
        }
        sleep(Duration::from_millis(500)).await;
    }

    if last_err.is_empty() {
        Err("Chrome did not start within 30 seconds".into())
    } else {
        Err(last_err)
    }
}

/// True when an error means "nothing is listening on that port yet", which is
/// transient while a browser starts. Mirrors the check in `cdp`; kept separate
/// so this module does not depend on the CDP client's internals.
fn is_connection_refused(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("connection refused")
        || m.contains("actively refused")
        || m.contains("os error 10061")
        || m.contains("os error 111")
}

/// Find Chrome/Chromium executable path.
///
/// Search order: known per-platform install locations first, then the PATH via
/// the same resolver `Aike.which` uses (`which_executable`), so both the
/// bundled-paths and the command-name lookups share one logic.
fn find_chrome() -> Option<String> {
    // Known install locations that are not on PATH.
    #[cfg(target_os = "macos")]
    {
        let paths = [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ];
        for p in &paths {
            if std::path::Path::new(p).exists() {
                return Some(p.to_string());
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let paths = [
            "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
            "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
            "C:\\Program Files\\Chromium\\Application\\chrome.exe",
            "C:\\Program Files\\BraveSoftware\\Brave-Browser\\Application\\brave.exe",
        ];
        for p in &paths {
            if std::path::Path::new(p).exists() {
                return Some(p.to_string());
            }
        }
    }

    // Fall back to searching PATH for the common command names.
    const CANDIDATES: &[&str] = &[
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "brave-browser",
        "chrome",
    ];
    CANDIDATES.iter().find_map(|name| which_executable(name))
}

/// Resolve an executable name against `PATH` (PATHEXT-aware on Windows),
/// returning the first existing candidate. Local to the extension because the
/// host's `Aike.which` resolver is not reachable through the zapi ABI.
fn which_executable(name: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;

    #[cfg(windows)]
    {
        let mut names = vec![name.to_string()];
        if std::path::Path::new(name).extension().is_none() {
            let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.BAT;.CMD".to_string());
            for ext in pathext.split(';').filter(|e| !e.is_empty()) {
                names.push(format!("{name}{ext}"));
            }
        }
        for dir in std::env::split_paths(&path_var) {
            for n in &names {
                let cand = dir.join(n);
                if cand.is_file() {
                    return Some(cand.to_string_lossy().into_owned());
                }
            }
        }
    }

    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in std::env::split_paths(&path_var) {
            let cand = dir.join(name);
            let is_exec = cand
                .metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
            if is_exec {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }

    None
}
