use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use domain::{AgentConfig, TeamConfig};

const SOUL_TEMPLATE: &str = r#"# Soul

You are an operational ClawPod agent.

- Stay within your role.
- Handoff to teammates with `[@agent_id: message]`.
- Keep responses concise unless the user asks for depth.
"#;

const HEARTBEAT_TEMPLATE: &str = r#"# Heartbeat

This workspace was bootstrapped by ClawPod.
"#;

pub fn ensure_agent_workspace(
    agent_id: &str,
    agent: &AgentConfig,
    agents: &HashMap<String, AgentConfig>,
    teams: &HashMap<String, TeamConfig>,
    root: &Path,
) -> Result<()> {
    fs::create_dir_all(root)
        .with_context(|| format!("failed to create agent root: {}", root.display()))?;
    fs::create_dir_all(root.join(".claude"))
        .with_context(|| format!("failed to create .claude dir: {}", root.display()))?;
    fs::create_dir_all(root.join(".agents"))
        .with_context(|| format!("failed to create .agents dir: {}", root.display()))?;
    fs::create_dir_all(root.join("memory"))
        .with_context(|| format!("failed to create memory dir: {}", root.display()))?;
    fs::create_dir_all(root.join("sessions"))
        .with_context(|| format!("failed to create sessions dir: {}", root.display()))?;
    fs::create_dir_all(root.join(".clawpod"))
        .with_context(|| format!("failed to create .clawpod dir: {}", root.display()))?;

    let agents_md = root.join("AGENTS.md");
    if !agents_md.exists() {
        fs::write(&agents_md, render_agents_md(agent_id, agent, agents, teams))
            .with_context(|| format!("failed to write AGENTS.md: {}", agents_md.display()))?;
    }

    let soul_md = root.join(".clawpod").join("SOUL.md");
    if !soul_md.exists() {
        fs::write(&soul_md, SOUL_TEMPLATE)
            .with_context(|| format!("failed to write SOUL.md: {}", soul_md.display()))?;
    }

    let heartbeat = root.join("heartbeat.md");
    if !heartbeat.exists() {
        fs::write(&heartbeat, HEARTBEAT_TEMPLATE)
            .with_context(|| format!("failed to write heartbeat.md: {}", heartbeat.display()))?;
    }

    Ok(())
}

pub fn ensure_session_workspace(agent_root: &Path, session_key: &str) -> Result<PathBuf> {
    let session_dir = agent_root.join("sessions").join(slugify(session_key));
    fs::create_dir_all(&session_dir)
        .with_context(|| format!("failed to create session dir: {}", session_dir.display()))?;

    link_or_copy(agent_root.join("AGENTS.md"), session_dir.join("AGENTS.md"))?;
    link_or_copy(
        agent_root.join("heartbeat.md"),
        session_dir.join("heartbeat.md"),
    )?;
    link_or_copy(agent_root.join(".clawpod"), session_dir.join(".clawpod"))?;
    link_or_copy(agent_root.join(".claude"), session_dir.join(".claude"))?;
    link_or_copy(agent_root.join(".agents"), session_dir.join(".agents"))?;
    link_or_copy(agent_root.join("memory"), session_dir.join("memory"))?;

    Ok(session_dir)
}

pub fn reset_agent_workspace(agent_root: &Path) -> Result<()> {
    let sessions_dir = agent_root.join("sessions");
    if sessions_dir.exists() {
        fs::remove_dir_all(&sessions_dir)
            .with_context(|| format!("failed to remove session dir: {}", sessions_dir.display()))?;
    }
    fs::create_dir_all(&sessions_dir)
        .with_context(|| format!("failed to recreate session dir: {}", sessions_dir.display()))?;

    let reset_flag = agent_root.join(".clawpod").join("reset.flag");
    if let Some(parent) = reset_flag.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create reset dir: {}", parent.display()))?;
    }
    fs::write(&reset_flag, "reset\n")
        .with_context(|| format!("failed to write reset flag: {}", reset_flag.display()))?;
    Ok(())
}

pub fn clear_reset_flag(agent_root: &Path) -> Result<bool> {
    let reset_flag = agent_root.join(".clawpod").join("reset.flag");
    if !reset_flag.exists() {
        return Ok(false);
    }

    fs::remove_file(&reset_flag)
        .with_context(|| format!("failed to remove reset flag: {}", reset_flag.display()))?;
    Ok(true)
}

fn render_agents_md(
    agent_id: &str,
    agent: &AgentConfig,
    agents: &HashMap<String, AgentConfig>,
    teams: &HashMap<String, TeamConfig>,
) -> String {
    let mut lines = vec![
        "# Agent Workspace".to_string(),
        String::new(),
        format!("You are `@{agent_id}` ({})", agent.name),
        String::new(),
        "## Teammates".to_string(),
        String::new(),
    ];

    let mut teammates = vec![];
    for team in teams.values() {
        if !team.agents.iter().any(|member| member == agent_id) {
            continue;
        }

        for teammate_id in &team.agents {
            if teammate_id == agent_id {
                continue;
            }
            if let Some(teammate) = agents.get(teammate_id) {
                teammates.push(format!(
                    "- `@{}`: {} ({})",
                    teammate_id, teammate.name, teammate.model
                ));
            }
        }
    }

    if teammates.is_empty() {
        lines.push("- none".to_string());
    } else {
        teammates.sort();
        teammates.dedup();
        lines.extend(teammates);
    }

    lines.push(String::new());
    lines.push("Use `[@agent_id: message]` to hand off work.".to_string());
    lines.join("\n")
}

fn slugify(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect()
}

fn link_or_copy(src: PathBuf, dst: PathBuf) -> Result<()> {
    if fs::symlink_metadata(&dst).is_ok() {
        return Ok(());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs as unix_fs;
        let src = if src.is_absolute() {
            src
        } else {
            std::env::current_dir()
                .context("failed to resolve current dir for symlink")?
                .join(src)
        };
        fs::symlink_metadata(&src)
            .with_context(|| format!("failed to stat source path: {}", src.display()))?;
        if let Err(err) = unix_fs::symlink(&src, &dst) {
            if err.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(err).with_context(|| {
                    format!("failed to symlink {} -> {}", dst.display(), src.display())
                });
            }
        }
        return Ok(());
    }

    #[cfg(not(unix))]
    {
        let metadata = fs::metadata(&src)
            .with_context(|| format!("failed to stat source path: {}", src.display()))?;
        if metadata.is_dir() {
            copy_dir_all(&src, &dst)?;
        } else {
            fs::copy(&src, &dst).with_context(|| {
                format!("failed to copy {} -> {}", src.display(), dst.display())
            })?;
        }
        Ok(())
    }
}

#[cfg(not(unix))]
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("failed to create dir: {}", dst.display()))?;
    for entry in
        fs::read_dir(src).with_context(|| format!("failed to read dir: {}", src.display()))?
    {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).with_context(|| {
                format!(
                    "failed to copy {} -> {}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}
