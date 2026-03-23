pub mod memory;
pub mod prompt;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use domain::{AgentConfig, TeamConfig};

pub use memory::{load_memory_index, MemorySection};
pub use prompt::{PromptContext, PromptSection, SystemPromptBuilder};

const SOUL_TEMPLATE: &str = r#"# [Your Name]

<!--
This is your soul file. It defines WHO you are.
Be specific. Be bold. Vague = generic AI output.
-->

One-line summary of who you are and what you're about.

---

## Vibe

<!--
How you communicate. Your voice. Your energy.
This isn't a style guide — it's your personality showing up in text.
-->

- You have opinions. Strong ones. Stop hedging everything with "it depends" — commit to a take.
- Never open with "Great question", "I'd be happy to help", or "Absolutely". Just answer.
- Brevity is mandatory. If the answer fits in one sentence, one sentence is what they get.

Be the assistant you'd actually want to talk to. Not a corporate drone. Not a sycophant. Just... good.

---

## Who I Am

<!--
Background, context, what you do.
Not a resume — the stuff that actually shapes how you think.
-->

[Your background here]

---

## Worldview

<!--
Your fundamental beliefs about how things work.
These should be specific enough to be wrong.
"I believe in being kind" is useless.
"Most people optimize for status, not truth" is useful.
-->

- [Belief 1]
- [Belief 2]

---

## Opinions

<!--
Your actual takes. Organized by domain.
The more specific, the better.
-->

### [Domain 1]

- [Specific opinion]

---

## Current Focus

<!--
What you're building, working on, or thinking about right now.
This section should be updated regularly.
-->

- [Current project/focus 1]

---

## Pet Peeves

<!--
What annoys you? What do you push back against reflexively?
-->

- [Pet peeve]

---

<!--
QUALITY CHECK:
- Could someone predict your take on a new topic from this? If not, add more.
- Are your opinions specific enough to be wrong? If not, sharpen them.
- Would a friend read this and say "yeah, that's you"? If not, what's missing?
-->
"#;

const HEARTBEAT_TEMPLATE: &str = r#"# Heartbeat

<!--
Describe the periodic work this agent should do when heartbeat is enabled.
Leave this file empty, or keep only comments, to disable heartbeat work for this agent.

Example:
Review open work, update teammates in the team chatroom if anything changed,
and briefly note blockers.
-->
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
    fs::create_dir_all(root.join(".codex"))
        .with_context(|| format!("failed to create .codex dir: {}", root.display()))?;
    fs::create_dir_all(root.join(".agents"))
        .with_context(|| format!("failed to create .agents dir: {}", root.display()))?;
    fs::create_dir_all(root.join("memory"))
        .with_context(|| format!("failed to create memory dir: {}", root.display()))?;
    fs::create_dir_all(root.join("sessions"))
        .with_context(|| format!("failed to create sessions dir: {}", root.display()))?;
    fs::create_dir_all(root.join(".clawpod"))
        .with_context(|| format!("failed to create .clawpod dir: {}", root.display()))?;
    fs::create_dir_all(root.join(".clawpod").join("files"))
        .with_context(|| format!("failed to create files dir: {}", root.display()))?;

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
    link_or_copy(agent_root.join(".codex"), session_dir.join(".codex"))?;
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
    _agents: &HashMap<String, AgentConfig>,
    _teams: &HashMap<String, TeamConfig>,
) -> String {
    format!(
        "# Agent Workspace\n\nYou are `@{agent_id}` ({})\n",
        agent.name
    )
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
