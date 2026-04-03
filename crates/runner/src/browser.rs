use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex as StdMutex};

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const BROWSER_READY_TIMEOUT: Duration = Duration::from_secs(12);
const BROWSER_READY_POLL_INTERVAL: Duration = Duration::from_millis(250);
const CDP_HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const CDP_WS_TIMEOUT: Duration = Duration::from_secs(2);

static BROWSER_HTTP_CLIENT: LazyLock<Client> = LazyLock::new(Client::new);
static BROWSER_ENSURE_LOCKS: LazyLock<StdMutex<HashMap<u16, Arc<Mutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserLaunchConfig {
    cdp_port: u16,
    profile_dir: PathBuf,
    display: Option<String>,
    home_dir: Option<PathBuf>,
    executable_path: PathBuf,
    open_url: String,
}

pub async fn ensure_browser_ready(metadata: &HashMap<String, String>) -> Result<()> {
    let Some(config) = BrowserLaunchConfig::from_metadata(metadata)? else {
        return Ok(());
    };

    let ensure_lock = browser_ensure_lock(config.cdp_port);
    let _guard = ensure_lock.lock().await;

    if is_browser_cdp_ready(config.cdp_port).await? {
        return Ok(());
    }

    if is_tcp_port_listening(config.cdp_port).await {
        bail!(
            "browser port {} is in use but not responding to CDP",
            config.cdp_port
        );
    }

    launch_browser(&config).await?;
    wait_for_browser_cdp_ready(config.cdp_port).await?;
    Ok(())
}

impl BrowserLaunchConfig {
    fn from_metadata(metadata: &HashMap<String, String>) -> Result<Option<Self>> {
        let Some(cdp_port_raw) = metadata.get("browser_cdp_port") else {
            return Ok(None);
        };
        let profile_dir = metadata
            .get("browser_profile_dir")
            .context("browser_profile_dir missing from run metadata")?;
        let cdp_port = cdp_port_raw
            .parse::<u16>()
            .with_context(|| format!("invalid browser_cdp_port: {cdp_port_raw}"))?;

        Ok(Some(Self {
            cdp_port,
            profile_dir: PathBuf::from(profile_dir),
            display: metadata.get("browser_display").cloned(),
            home_dir: metadata.get("browser_home_dir").map(PathBuf::from),
            executable_path: resolve_browser_executable()?,
            open_url: std::env::var("AGENT_BROWSER_OPEN_URL")
                .unwrap_or_else(|_| "about:blank".to_string()),
        }))
    }
}

fn browser_ensure_lock(port: u16) -> Arc<Mutex<()>> {
    let mut locks = BROWSER_ENSURE_LOCKS
        .lock()
        .expect("browser ensure locks poisoned");
    locks
        .entry(port)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

async fn launch_browser(config: &BrowserLaunchConfig) -> Result<()> {
    tokio::fs::create_dir_all(&config.profile_dir)
        .await
        .with_context(|| format!("failed to create {}", config.profile_dir.display()))?;

    let mut command = Command::new(&config.executable_path);
    command
        .args(browser_launch_args(config))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if let Some(display) = &config.display {
        command.env("DISPLAY", display);
    }
    if let Some(home_dir) = &config.home_dir {
        command.env("HOME", home_dir);
        command.env("XDG_CONFIG_HOME", home_dir.join(".config"));
        command.env("XDG_CACHE_HOME", home_dir.join(".cache"));
        command.env("XDG_STATE_HOME", home_dir.join(".local/state"));
        command.env("XDG_DATA_HOME", home_dir.join(".local/share"));
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch {}", config.executable_path.display()))?;
    let pid = child.id().unwrap_or_default();
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    tracing::info!(
        browser_cdp_port = config.cdp_port,
        browser_profile_dir = %config.profile_dir.display(),
        browser_pid = pid,
        "launched browser for clawpod profile"
    );
    Ok(())
}

fn browser_launch_args(config: &BrowserLaunchConfig) -> Vec<String> {
    let mut args = vec![
        format!("--remote-debugging-port={}", config.cdp_port),
        format!("--user-data-dir={}", config.profile_dir.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-sync".to_string(),
        "--disable-background-networking".to_string(),
        "--disable-component-update".to_string(),
        "--disable-features=Translate,MediaRouter".to_string(),
        "--disable-session-crashed-bubble".to_string(),
        "--hide-crash-restore-bubble".to_string(),
        "--password-store=basic".to_string(),
    ];

    if cfg!(target_os = "linux") {
        args.push("--disable-dev-shm-usage".to_string());
        args.push("--disable-gpu".to_string());
        if current_uid() == Some(0) {
            args.push("--no-sandbox".to_string());
            args.push("--disable-setuid-sandbox".to_string());
        }
    }

    args.push(config.open_url.clone());
    args
}

fn resolve_browser_executable() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AGENT_BROWSER_EXECUTABLE_PATH").filter(|v| !v.is_empty())
    {
        return ensure_executable(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("CHROME_BIN").filter(|v| !v.is_empty()) {
        return ensure_executable(PathBuf::from(path));
    }

    let mut candidates = Vec::new();
    if cfg!(target_os = "macos") {
        candidates.extend([
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ]);
    }

    candidates.extend([
        "google-chrome",
        "chromium",
        "chromium-browser",
        "brave-browser",
        "microsoft-edge",
    ]);

    for candidate in candidates {
        if let Some(path) = resolve_executable_candidate(candidate) {
            return Ok(path);
        }
    }

    bail!("no supported Chrome/Chromium executable found for built-in browser launch")
}

fn ensure_executable(path: PathBuf) -> Result<PathBuf> {
    if path.is_file() {
        Ok(path)
    } else {
        bail!("browser executable not found: {}", path.display())
    }
}

fn resolve_executable_candidate(candidate: &str) -> Option<PathBuf> {
    let path = PathBuf::from(candidate);
    if path.is_absolute() {
        return path.is_file().then_some(path);
    }
    let search_path = std::env::var_os("PATH")?;
    std::env::split_paths(&search_path)
        .map(|entry| entry.join(candidate))
        .find(|path| path.is_file())
}

async fn wait_for_browser_cdp_ready(cdp_port: u16) -> Result<()> {
    let deadline = tokio::time::Instant::now() + BROWSER_READY_TIMEOUT;
    loop {
        if is_browser_cdp_ready(cdp_port).await? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("browser CDP on port {cdp_port} is not reachable after start");
        }
        sleep(BROWSER_READY_POLL_INTERVAL).await;
    }
}

async fn is_browser_cdp_ready(cdp_port: u16) -> Result<bool> {
    let Some(ws_url) = fetch_browser_ws_url(cdp_port).await? else {
        return Ok(false);
    };
    run_cdp_health_check(&ws_url).await
}

async fn fetch_browser_ws_url(cdp_port: u16) -> Result<Option<String>> {
    let request = BROWSER_HTTP_CLIENT.get(format!("http://127.0.0.1:{cdp_port}/json/version"));
    let response = match timeout(CDP_HTTP_TIMEOUT, request.send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(_)) | Err(_) => return Ok(None),
    };
    if !response.status().is_success() {
        return Ok(None);
    }

    let value = response
        .json::<Value>()
        .await
        .with_context(|| format!("failed to parse /json/version for port {cdp_port}"))?;
    Ok(value
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string))
}

async fn run_cdp_health_check(ws_url: &str) -> Result<bool> {
    let connect = timeout(CDP_WS_TIMEOUT, connect_async(ws_url)).await;
    let (mut websocket, _) = match connect {
        Ok(Ok(result)) => result,
        Ok(Err(_)) | Err(_) => return Ok(false),
    };

    websocket
        .send(Message::Text(
            r#"{"id":1,"method":"Browser.getVersion"}"#.to_string().into(),
        ))
        .await
        .with_context(|| format!("failed to write CDP health probe to {ws_url}"))?;

    loop {
        let next = timeout(CDP_WS_TIMEOUT, websocket.next()).await;
        let message = match next {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(_))) | Ok(None) | Err(_) => return Ok(false),
        };

        match message {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text)
                    .with_context(|| format!("invalid CDP probe response from {ws_url}"))?;
                if value.get("id").and_then(Value::as_i64) == Some(1) {
                    return Ok(value.get("result").is_some());
                }
            }
            Message::Close(_) => return Ok(false),
            _ => {}
        }
    }
}

async fn is_tcp_port_listening(port: u16) -> bool {
    timeout(
        Duration::from_millis(250),
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .is_ok_and(|result| result.is_ok())
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    Some(unsafe { libc::geteuid() })
}

#[cfg(not(unix))]
fn current_uid() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::{browser_launch_args, BrowserLaunchConfig};
    use std::path::PathBuf;

    #[test]
    fn launch_args_include_required_browser_flags() {
        let config = BrowserLaunchConfig {
            cdp_port: 9410,
            profile_dir: PathBuf::from("/tmp/clawpod-browser"),
            display: Some(":11".to_string()),
            home_dir: None,
            executable_path: PathBuf::from("/usr/bin/google-chrome"),
            open_url: "about:blank".to_string(),
        };

        let args = browser_launch_args(&config);
        assert!(args.contains(&"--remote-debugging-port=9410".to_string()));
        assert!(args.contains(&"--user-data-dir=/tmp/clawpod-browser".to_string()));
        assert!(args.contains(&"about:blank".to_string()));
    }
}
