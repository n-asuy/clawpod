pub mod consolidation;
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

/// Subdirectories inside `.claude/` and `.agents/` that should be shared
/// (symlinked) across sessions.  Everything else inside these directories is
/// session-specific so that Claude Code's auto-memory and per-session state
/// do not leak between conversations.
const SHARED_CLAUDE_SUBDIRS: &[&str] = &["skills", "settings"];
const SHARED_AGENTS_SUBDIRS: &[&str] = &["skills"];

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

    // .claude/ — create as a real directory, selectively symlink shared subdirs
    selective_link_dir(agent_root, &session_dir, ".claude", SHARED_CLAUDE_SUBDIRS)?;

    link_or_copy(agent_root.join(".codex"), session_dir.join(".codex"))?;

    // .agents/ — create as a real directory, selectively symlink shared subdirs
    selective_link_dir(agent_root, &session_dir, ".agents", SHARED_AGENTS_SUBDIRS)?;

    link_or_copy(agent_root.join("memory"), session_dir.join("memory"))?;

    Ok(session_dir)
}

/// Create `<session_dir>/<dir_name>/` as a real directory and symlink only
/// the listed subdirectories from `<agent_root>/<dir_name>/<sub>`.
fn selective_link_dir(
    agent_root: &Path,
    session_dir: &Path,
    dir_name: &str,
    shared_subdirs: &[&str],
) -> Result<()> {
    let session_target = session_dir.join(dir_name);

    // Remove stale symlink from old layout (whole-dir symlink) before creating
    // a real directory with selective sub-symlinks.
    if fs::symlink_metadata(&session_target)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        fs::remove_file(&session_target).with_context(|| {
            format!(
                "failed to remove stale symlink: {}",
                session_target.display()
            )
        })?;
    }

    fs::create_dir_all(&session_target)
        .with_context(|| format!("failed to create {} dir: {}", dir_name, session_target.display()))?;

    for &sub in shared_subdirs {
        let src = agent_root.join(dir_name).join(sub);
        if src.exists() {
            link_or_copy(src, session_target.join(sub))?;
        }
    }
    Ok(())
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
    // Check if dst already exists.  `symlink_metadata` succeeds even for
    // broken (dangling) symlinks, so we additionally verify the target is
    // reachable via `fs::metadata` which follows symlinks.
    if let Ok(meta) = fs::symlink_metadata(&dst) {
        if meta.file_type().is_symlink() {
            if fs::metadata(&dst).is_err() {
                // Dangling symlink — remove it so we can recreate below.
                fs::remove_file(&dst).with_context(|| {
                    format!("failed to remove dangling symlink: {}", dst.display())
                })?;
            } else {
                return Ok(());
            }
        } else {
            // Real file/dir already exists.
            return Ok(());
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs as unix_fs;
        let abs_src = if src.is_absolute() {
            src
        } else {
            std::env::current_dir()
                .context("failed to resolve current dir for symlink")?
                .join(src)
        };
        fs::symlink_metadata(&abs_src)
            .with_context(|| format!("failed to stat source path: {}", abs_src.display()))?;

        // Use relative symlink so links survive workspace moves / user changes.
        let link_target = match dst.parent() {
            Some(dst_parent) => relative_path(dst_parent, &abs_src),
            None => abs_src,
        };

        if let Err(err) = unix_fs::symlink(&link_target, &dst) {
            if err.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(err).with_context(|| {
                    format!("failed to symlink {} -> {}", dst.display(), link_target.display())
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

/// Compute a relative path from `base` to `target`.
///
/// Both paths must be absolute. Returns a `PathBuf` like `../../memory`
/// that, when resolved from `base`, reaches `target`.
fn relative_path(base: &Path, target: &Path) -> PathBuf {
    let base_parts: Vec<_> = base.components().collect();
    let target_parts: Vec<_> = target.components().collect();

    // Find common prefix length.
    let common = base_parts
        .iter()
        .zip(target_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut rel = PathBuf::new();
    for _ in common..base_parts.len() {
        rel.push("..");
    }
    for part in &target_parts[common..] {
        rel.push(part);
    }
    rel
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_sibling() {
        let base = Path::new("/a/b/c");
        let target = Path::new("/a/b/d");
        assert_eq!(relative_path(base, target), PathBuf::from("../d"));
    }

    #[test]
    fn relative_path_parent() {
        let base = Path::new("/a/b/sessions/key");
        let target = Path::new("/a/b/memory");
        assert_eq!(
            relative_path(base, target),
            PathBuf::from("../../memory")
        );
    }

    #[test]
    fn relative_path_same_dir() {
        let base = Path::new("/a/b");
        let target = Path::new("/a/b/file.md");
        assert_eq!(relative_path(base, target), PathBuf::from("file.md"));
    }

    #[test]
    fn relative_path_deeply_nested() {
        let base = Path::new("/a/b/c/d");
        let target = Path::new("/a/x/y");
        assert_eq!(
            relative_path(base, target),
            PathBuf::from("../../../x/y")
        );
    }

    #[cfg(unix)]
    #[test]
    fn session_symlinks_are_relative() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut agents = HashMap::new();
        agents.insert(
            "bot".to_string(),
            AgentConfig {
                name: "Bot".into(),
                provider: domain::ProviderKind::Anthropic,
                model: "sonnet".into(),
                think_level: None,
                provider_id: None,
                system_prompt: None,
                prompt_file: None,
                heartbeat: None,
            },
        );
        let teams = HashMap::new();

        ensure_agent_workspace("bot", &agents["bot"], &agents, &teams, root).unwrap();
        let session_dir = ensure_session_workspace(root, "test:session").unwrap();

        // memory symlink should be relative
        let memory_link = session_dir.join("memory");
        let link_target = fs::read_link(&memory_link).unwrap();
        assert!(
            link_target.is_relative(),
            "memory symlink should be relative, got: {}",
            link_target.display()
        );
        // And it should resolve correctly
        assert!(
            fs::metadata(&memory_link).is_ok(),
            "relative symlink should resolve to existing directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn link_or_copy_repairs_dangling_symlink() {
        use std::os::unix::fs as unix_fs;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create a real source directory.
        let src = root.join("real_dir");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("file.txt"), "hello").unwrap();

        // Create a dangling symlink at dst pointing to a non-existent target.
        let dst = root.join("link");
        unix_fs::symlink("/nonexistent/path", &dst).unwrap();

        // Verify the symlink is dangling.
        assert!(fs::symlink_metadata(&dst).is_ok(), "symlink itself exists");
        assert!(fs::metadata(&dst).is_err(), "target does not exist");

        // link_or_copy should repair the dangling symlink.
        link_or_copy(src.clone(), dst.clone()).unwrap();

        // Now the symlink should resolve to the real source.
        assert!(
            fs::metadata(&dst).is_ok(),
            "repaired symlink should resolve"
        );
        assert!(
            fs::metadata(dst.join("file.txt")).is_ok(),
            "contents should be accessible through repaired link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn link_or_copy_skips_healthy_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let src = root.join("src_dir");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.txt"), "aaa").unwrap();

        let dst = root.join("dst_link");

        // First call creates the symlink.
        link_or_copy(src.clone(), dst.clone()).unwrap();
        let original_target = fs::read_link(&dst).unwrap();

        // Second call should be a no-op (not recreate).
        link_or_copy(src, dst.clone()).unwrap();
        let target_after = fs::read_link(&dst).unwrap();

        assert_eq!(original_target, target_after);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_session_workspace_repairs_dangling_memory_link() {
        use std::os::unix::fs as unix_fs;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut agents = HashMap::new();
        agents.insert(
            "bot".to_string(),
            AgentConfig {
                name: "Bot".into(),
                provider: domain::ProviderKind::Anthropic,
                model: "sonnet".into(),
                think_level: None,
                provider_id: None,
                system_prompt: None,
                prompt_file: None,
                heartbeat: None,
            },
        );
        let teams = HashMap::new();

        ensure_agent_workspace("bot", &agents["bot"], &agents, &teams, root).unwrap();
        let session_dir = ensure_session_workspace(root, "test:repair").unwrap();

        // Break the memory symlink by replacing it with a dangling one.
        let memory_link = session_dir.join("memory");
        fs::remove_file(&memory_link).unwrap();
        unix_fs::symlink("/nonexistent/memory", &memory_link).unwrap();
        assert!(fs::metadata(&memory_link).is_err(), "link is now dangling");

        // Re-running ensure_session_workspace should repair it.
        let session_dir2 = ensure_session_workspace(root, "test:repair").unwrap();
        assert_eq!(session_dir, session_dir2);

        let repaired = session_dir2.join("memory");
        assert!(
            fs::metadata(&repaired).is_ok(),
            "memory symlink should be repaired and resolve"
        );
    }
}
