use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use config::{ResolvedBrowserProfile, RuntimeConfig};

const MACOS_SERVICE_LABEL: &str = "com.clawpod.daemon";
const LINUX_SERVICE_UNIT: &str = "clawpod.service";
const LINUX_BROWSER_UNIT_PREFIX: &str = "clawpod-kasmvnc-";

pub fn handle_command(
    command: &crate::ServiceCommand,
    config: &RuntimeConfig,
    config_path: &Path,
) -> Result<()> {
    match command {
        crate::ServiceCommand::Install => install(config, config_path),
        crate::ServiceCommand::Start => start(config),
        crate::ServiceCommand::Stop => stop(config),
        crate::ServiceCommand::Restart => restart(config),
        crate::ServiceCommand::Status => status(config),
        crate::ServiceCommand::Uninstall => uninstall(config),
    }
}

fn install(config: &RuntimeConfig, config_path: &Path) -> Result<()> {
    if cfg!(target_os = "macos") {
        install_macos(config, config_path)
    } else if cfg!(target_os = "linux") {
        install_linux(config, config_path)
    } else {
        bail!("service management is supported on macOS and Linux only");
    }
}

fn start(config: &RuntimeConfig) -> Result<()> {
    if cfg!(target_os = "macos") {
        let plist = macos_service_file()?;
        run_checked(Command::new("launchctl").arg("load").arg("-w").arg(&plist))?;
        run_checked(
            Command::new("launchctl")
                .arg("start")
                .arg(MACOS_SERVICE_LABEL),
        )?;
        println!("service started");
        println!("stdout: {}", config.daemon_log_path().display());
        println!("stderr: {}", config.daemon_stderr_path().display());
        Ok(())
    } else if cfg!(target_os = "linux") {
        run_checked(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
        run_checked(Command::new("systemctl").args(["--user", "start", LINUX_SERVICE_UNIT]))?;
        for profile in config.resolved_browser_profiles()? {
            run_checked(Command::new("systemctl").args([
                "--user",
                "start",
                &linux_browser_unit_name(&profile.name),
            ]))?;
        }
        println!("service started");
        println!("stdout: {}", config.daemon_log_path().display());
        println!("stderr: {}", config.daemon_stderr_path().display());
        Ok(())
    } else {
        bail!("service management is supported on macOS and Linux only");
    }
}

fn stop(config: &RuntimeConfig) -> Result<()> {
    if cfg!(target_os = "macos") {
        let plist = macos_service_file()?;
        let _ = run_checked(
            Command::new("launchctl")
                .arg("stop")
                .arg(MACOS_SERVICE_LABEL),
        );
        let _ = run_checked(
            Command::new("launchctl")
                .arg("unload")
                .arg("-w")
                .arg(&plist),
        );
        println!("service stopped");
        Ok(())
    } else if cfg!(target_os = "linux") {
        for profile in config.resolved_browser_profiles()? {
            let _ = run_checked(Command::new("systemctl").args([
                "--user",
                "stop",
                &linux_browser_unit_name(&profile.name),
            ]));
        }
        let _ = run_checked(Command::new("systemctl").args(["--user", "stop", LINUX_SERVICE_UNIT]));
        println!("service stopped");
        Ok(())
    } else {
        bail!("service management is supported on macOS and Linux only");
    }
}

fn restart(config: &RuntimeConfig) -> Result<()> {
    stop(config)?;
    start(config)?;
    println!("service restarted");
    Ok(())
}

fn status(config: &RuntimeConfig) -> Result<()> {
    if cfg!(target_os = "macos") {
        let out = run_capture(Command::new("launchctl").arg("list"))?;
        let running = out.lines().any(|line| line.contains(MACOS_SERVICE_LABEL));
        println!(
            "service: {}",
            if running {
                "running/loaded"
            } else {
                "not loaded"
            }
        );
        println!("unit: {}", macos_service_file()?.display());
        println!("stdout: {}", config.daemon_log_path().display());
        println!("stderr: {}", config.daemon_stderr_path().display());
        return Ok(());
    }

    if cfg!(target_os = "linux") {
        let out = run_capture(Command::new("systemctl").args([
            "--user",
            "is-active",
            LINUX_SERVICE_UNIT,
        ]))
        .unwrap_or_else(|_| "inactive".to_string());
        println!("service: {}", out.trim());
        println!("unit: {}", linux_service_file()?.display());
        println!("stdout: {}", config.daemon_log_path().display());
        println!("stderr: {}", config.daemon_stderr_path().display());
        let profiles = config.resolved_browser_profiles()?;
        if !profiles.is_empty() {
            println!("browser viewers:");
            for profile in profiles {
                let status = run_capture(Command::new("systemctl").args([
                    "--user",
                    "is-active",
                    &linux_browser_unit_name(&profile.name),
                ]))
                .unwrap_or_else(|_| "inactive".to_string());
                println!(
                    "  {name}: {status}  display={display}  websocket={port}  view={view}",
                    name = profile.name,
                    status = status.trim(),
                    display = profile.display,
                    port = profile.kasm_port,
                    view = profile.view_path,
                );
            }
        }
        return Ok(());
    }

    bail!("service management is supported on macOS and Linux only")
}

fn uninstall(config: &RuntimeConfig) -> Result<()> {
    if cfg!(target_os = "macos") {
        let plist = macos_service_file()?;
        let _ = stop(config);
        if plist.exists() {
            fs::remove_file(&plist)
                .with_context(|| format!("failed to remove {}", plist.display()))?;
        }
        println!("service uninstalled");
        return Ok(());
    }

    if cfg!(target_os = "linux") {
        let _ = stop(config);
        let unit = linux_service_file()?;
        if unit.exists() {
            fs::remove_file(&unit)
                .with_context(|| format!("failed to remove {}", unit.display()))?;
        }
        for profile in config.resolved_browser_profiles()? {
            let unit = linux_browser_service_file(&profile.name)?;
            if unit.exists() {
                fs::remove_file(&unit)
                    .with_context(|| format!("failed to remove {}", unit.display()))?;
            }
        }
        run_checked(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
        println!("service uninstalled");
        return Ok(());
    }

    bail!("service management is supported on macOS and Linux only")
}

fn install_macos(config: &RuntimeConfig, config_path: &Path) -> Result<()> {
    let plist_path = macos_service_file()?;
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let program = env::current_exe().context("failed to resolve current executable")?;
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{program}</string>
    <string>--config</string>
    <string>{config_path}</string>
    <string>--log-level</string>
    <string>info</string>
    <string>daemon</string>
  </array>
  <key>KeepAlive</key>
  <true/>
  <key>RunAtLoad</key>
  <true/>
  <key>WorkingDirectory</key>
  <string>{workdir}</string>
  <key>StandardOutPath</key>
  <string>{stdout_path}</string>
  <key>StandardErrorPath</key>
  <string>{stderr_path}</string>
</dict>
</plist>
"#,
        label = MACOS_SERVICE_LABEL,
        program = xml_escape(&program.display().to_string()),
        config_path = xml_escape(&config_path.display().to_string()),
        workdir = xml_escape(&config.home_dir().display().to_string()),
        stdout_path = xml_escape(&config.daemon_log_path().display().to_string()),
        stderr_path = xml_escape(&config.daemon_stderr_path().display().to_string()),
    );
    fs::write(&plist_path, plist)
        .with_context(|| format!("failed to write {}", plist_path.display()))?;
    println!("service installed");
    println!("unit: {}", plist_path.display());
    Ok(())
}

fn install_linux(config: &RuntimeConfig, config_path: &Path) -> Result<()> {
    let unit_path = linux_service_file()?;
    if let Some(parent) = unit_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let program = env::current_exe().context("failed to resolve current executable")?;
    let unit = format!(
        r#"[Unit]
Description=ClawPod daemon
After=network.target

[Service]
Type=simple
ExecStart={program} --config {config_path} --log-level info daemon
WorkingDirectory={workdir}
Restart=always
RestartSec=3
StandardOutput=append:{stdout_path}
StandardError=append:{stderr_path}

[Install]
WantedBy=default.target
"#,
        program = program.display(),
        config_path = config_path.display(),
        workdir = config.home_dir().display(),
        stdout_path = config.daemon_log_path().display(),
        stderr_path = config.daemon_stderr_path().display(),
    );
    fs::write(&unit_path, unit)
        .with_context(|| format!("failed to write {}", unit_path.display()))?;
    let browser_unit_paths = install_linux_browser_viewers(config)?;
    run_checked(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
    println!("service installed");
    println!("unit: {}", unit_path.display());
    for path in browser_unit_paths {
        println!("viewer unit: {}", path.display());
    }
    Ok(())
}

fn macos_service_file() -> Result<PathBuf> {
    let home = env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{MACOS_SERVICE_LABEL}.plist")))
}

fn linux_service_file() -> Result<PathBuf> {
    let home = env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("systemd")
        .join("user")
        .join(LINUX_SERVICE_UNIT))
}

fn install_linux_browser_viewers(config: &RuntimeConfig) -> Result<Vec<PathBuf>> {
    let profiles = config.resolved_browser_profiles()?;
    let mut unit_paths = Vec::with_capacity(profiles.len());
    for profile in profiles {
        let unit_path = linux_browser_service_file(&profile.name)?;
        if let Some(parent) = unit_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let unit = render_linux_browser_unit(config, &profile)?;
        fs::write(&unit_path, unit)
            .with_context(|| format!("failed to write {}", unit_path.display()))?;
        unit_paths.push(unit_path);
    }
    Ok(unit_paths)
}

fn render_linux_browser_unit(
    config: &RuntimeConfig,
    profile: &ResolvedBrowserProfile,
) -> Result<String> {
    let home_dir = profile
        .home_dir
        .clone()
        .unwrap_or_else(|| config.home_dir());
    let workdir = home_dir.display().to_string();
    let stdout_path = browser_log_path(config, &profile.name)
        .display()
        .to_string();
    let stderr_path = browser_stderr_path(config, &profile.name)
        .display()
        .to_string();
    Ok(format!(
        r#"[Unit]
Description=ClawPod KasmVNC viewer ({profile_name})
After=network.target

[Service]
Type=forking
Environment=HOME={home_dir}
ExecStartPre=/usr/bin/bash -lc '/usr/bin/vncserver -kill {display} 2>/dev/null || true'
ExecStart=/usr/bin/vncserver {display} -geometry 1280x720 -depth 24 -websocketPort {kasm_port} -SecurityTypes None -DisableBasicAuth 1
ExecStop=/usr/bin/vncserver -kill {display}
WorkingDirectory={workdir}
Restart=always
RestartSec=3
StandardOutput=append:{stdout_path}
StandardError=append:{stderr_path}

[Install]
WantedBy=default.target
"#,
        profile_name = profile.name,
        home_dir = home_dir.display(),
        display = profile.display,
        kasm_port = profile.kasm_port,
        workdir = workdir,
        stdout_path = stdout_path,
        stderr_path = stderr_path,
    ))
}

fn linux_browser_unit_name(profile_name: &str) -> String {
    format!("{LINUX_BROWSER_UNIT_PREFIX}{profile_name}.service")
}

fn linux_browser_service_file(profile_name: &str) -> Result<PathBuf> {
    let home = env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("systemd")
        .join("user")
        .join(linux_browser_unit_name(profile_name)))
}

fn browser_log_path(config: &RuntimeConfig, profile_name: &str) -> PathBuf {
    config
        .home_dir()
        .join("logs")
        .join(format!("kasmvnc-{profile_name}.log"))
}

fn browser_stderr_path(config: &RuntimeConfig, profile_name: &str) -> PathBuf {
    config
        .home_dir()
        .join("logs")
        .join(format!("kasmvnc-{profile_name}.stderr.log"))
}

fn run_checked(command: &mut Command) -> Result<()> {
    let status = command.status().context("failed to run command")?;
    if status.success() {
        Ok(())
    } else {
        bail!("command exited with status {:?}", status.code())
    }
}

fn run_capture(command: &mut Command) -> Result<String> {
    let output = command.output().context("failed to run command")?;
    if !output.status.success() {
        bail!("command exited with status {:?}", output.status.code());
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        Ok(stdout)
    } else {
        Ok(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
