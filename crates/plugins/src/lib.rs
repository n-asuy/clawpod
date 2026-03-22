use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use config::RuntimeConfig;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookContext {
    pub channel: String,
    pub sender: String,
    pub sender_id: Option<String>,
    pub message_id: String,
    pub original_message: String,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookResult {
    pub text: String,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PluginManifest {
    name: String,
    incoming: Option<String>,
    outgoing: Option<String>,
    event: Option<String>,
}

pub async fn transform_incoming(
    config: &RuntimeConfig,
    text: &str,
    context: &HookContext,
) -> Result<HookResult> {
    run_transform_hooks(config, "incoming", text, context).await
}

pub async fn transform_outgoing(
    config: &RuntimeConfig,
    text: &str,
    context: &HookContext,
) -> Result<HookResult> {
    run_transform_hooks(config, "outgoing", text, context).await
}

pub async fn dispatch_event(
    config: &RuntimeConfig,
    event_type: &str,
    payload: &Value,
) -> Result<()> {
    for (dir, plugin) in load_plugins(&config.home_dir().join("plugins"))? {
        let Some(command) = plugin.event.clone() else {
            continue;
        };
        let hook_payload = json!({
            "plugin": plugin.name,
            "event_type": event_type,
            "payload": payload,
        });
        if let Err(err) = run_command(&dir, &command, &hook_payload).await {
            warn!("plugin event hook failed for {}: {err:#}", plugin.name);
        }
    }

    Ok(())
}

async fn run_transform_hooks(
    config: &RuntimeConfig,
    hook_kind: &str,
    text: &str,
    context: &HookContext,
) -> Result<HookResult> {
    let mut current = HookResult {
        text: text.to_string(),
        metadata: HashMap::new(),
    };

    for (dir, plugin) in load_plugins(&config.home_dir().join("plugins"))? {
        let command = match hook_kind {
            "incoming" => plugin.incoming.clone(),
            "outgoing" => plugin.outgoing.clone(),
            _ => None,
        };
        let Some(command) = command else {
            continue;
        };

        let payload = json!({
            "plugin": plugin.name,
            "hook": hook_kind,
            "text": current.text,
            "metadata": current.metadata,
            "context": context,
        });

        match run_command(&dir, &command, &payload).await {
            Ok(Some(result)) => current = result,
            Ok(None) => {}
            Err(err) => warn!(
                "plugin {hook_kind} hook failed for {}: {err:#}",
                plugin.name
            ),
        }
    }

    Ok(current)
}

fn load_plugins(root: &Path) -> Result<Vec<(PathBuf, PluginManifest)>> {
    if !root.exists() {
        return Ok(vec![]);
    }

    let mut plugins = vec![];
    for entry in fs::read_dir(root)
        .with_context(|| format!("failed to read plugin dir: {}", root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let manifest_path = path.join("plugin.toml");
        if !manifest_path.exists() {
            continue;
        }
        let raw = fs::read_to_string(&manifest_path).with_context(|| {
            format!(
                "failed to read plugin manifest: {}",
                manifest_path.display()
            )
        })?;
        let plugin: PluginManifest = toml::from_str(&raw)
            .with_context(|| format!("invalid plugin manifest: {}", manifest_path.display()))?;
        plugins.push((path, plugin));
    }
    Ok(plugins)
}

async fn run_command(
    working_dir: &Path,
    command: &str,
    payload: &Value,
) -> Result<Option<HookResult>> {
    let mut process = shell_command(command);
    process.current_dir(working_dir);
    process.stdin(std::process::Stdio::piped());
    process.stdout(std::process::Stdio::piped());
    process.stderr(std::process::Stdio::piped());

    let mut child = process
        .spawn()
        .with_context(|| format!("failed to spawn plugin command: {command}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(payload.to_string().as_bytes())
            .await
            .context("failed to write plugin stdin")?;
    }

    let output = child
        .wait_with_output()
        .await
        .with_context(|| format!("failed to execute plugin command: {command}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!("plugin command failed: {command}: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Ok(None);
    }

    if let Ok(value) = serde_json::from_str::<Value>(&stdout) {
        if let Some(text) = value.get("text").and_then(Value::as_str) {
            let metadata = value
                .get("metadata")
                .and_then(Value::as_object)
                .map(|object| {
                    object
                        .iter()
                        .filter_map(|(key, value)| {
                            value.as_str().map(|v| (key.clone(), v.to_string()))
                        })
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default();
            return Ok(Some(HookResult {
                text: text.to_string(),
                metadata,
            }));
        }
    }

    Ok(Some(HookResult {
        text: stdout,
        metadata: HashMap::new(),
    }))
}

fn shell_command(command: &str) -> Command {
    #[cfg(unix)]
    {
        let mut cmd = Command::new("sh");
        cmd.args(["-lc", command]);
        cmd
    }

    #[cfg(not(unix))]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    }
}
