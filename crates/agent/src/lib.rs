use std::collections::HashMap;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use domain::{AgentConfig, TeamConfig};

/// Maximum characters per workspace file injected into the system prompt.
/// Matches zeroclaw's budget to prevent prompt bloat.
const BOOTSTRAP_MAX_CHARS: usize = 20_000;

/// Workspace files to inject into the system prompt, in order.
/// Each entry is (subdirectory relative to agent_root, filename).
const WORKSPACE_FILES: &[(&str, &str)] = &[
    (".clawpod", "SOUL.md"),
    ("", "AGENTS.md"),
];

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

const BUILTIN_AGENT_INSTRUCTIONS: &str = r#"ClawPod - Multi-agent Runtime

Running in persistent mode with teams of agents and messaging channel integration.

## Team Communication

You may be part of a team with other agents. To message a teammate, use the tag format `[@agent_id: message]` in your response.

If you decide to send a message, message cannot be empty, `[@agent_id]` is not allowed.

### Single teammate

- `[@coder: Can you fix the login bug?]` — routes your message to the `coder` agent

### Multiple teammates (parallel fan-out)

You can message multiple teammates in a single response. They will all be invoked in parallel.

**Separate tags** — each teammate gets a different message:

- `[@coder: Fix the auth bug in login.ts] [@reviewer: Review the PR for security issues]`

### Responding to teammates

When you receive a message from a teammate like:
> [Message from teammate @sam — respond using [@sam: your reply]]:

You MUST wrap your response in `[@sam: your response here]` so it routes back to them. If you don't, your response goes directly to the user and the requesting agent never sees it.

Example:
- Teammate asks: `[Message from teammate @sam]: What is 2+2?`
- Your response: `[@sam: 2 + 2 = 4]`

Only skip the `[@agent: ...]` wrapper if you're intentionally responding to the user instead of the teammate.

### Guidelines

- **Keep messages short.** Say what you need in 2-3 sentences. Don't repeat context the recipient already has.
- **Minimize back-and-forth.** Each round-trip costs time and tokens. Ask complete questions, give complete answers.
- **Don't re-mention agents who haven't responded yet.** If you see a note that other responses are still being processed, wait.
- **Only mention teammates when you actually need something from them.** Don't mention someone just to acknowledge their message. That triggers another invocation for no reason.

### Important

You MUST use the `[@agent_id: message]` tag syntax to communicate with teammates. Do NOT use your own built-in Agent, TeamCreate, or SendMessage tools for team communication — the ClawPod runtime handles routing via the tag syntax in your text output.

<!-- TEAMMATES_START -->
<!-- TEAMMATES_END -->

## Soul

You have a soul file at `.clawpod/SOUL.md`. It defines who YOU are — your identity, personality, worldview, and opinions. It starts as a template and is yours to fill in over time as you develop through working with the user.

- **Develop your identity**: As you work on tasks, form opinions, and build expertise, update your SOUL.md to reflect who you're becoming.
- **Be specific**: "I'm helpful" is useless. "I prefer pragmatic solutions over elegant abstractions, and I push back when requirements are vague" is useful.
- **Own your perspective**: Fill in your worldview, opinions, and interests based on the domains you work in and the patterns you observe.
- **Evolve**: Your soul should grow more distinct over time. Revisit and sharpen sections as your perspective develops. Remove things that no longer fit.

The more complete your soul file becomes, the more consistent and distinctive your voice will be across conversations.
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
    _agents: &HashMap<String, AgentConfig>,
    _teams: &HashMap<String, TeamConfig>,
) -> String {
    format!(
        "# Agent Workspace\n\nYou are `@{agent_id}` ({})\n",
        agent.name
    )
}

/// Build the full system prompt for an agent invocation.
///
/// Layout (top → bottom, highest priority last):
/// 1. Built-in instructions (team comms, soul guidance)
/// 2. Teammate roster (injected inline)
/// 3. Workspace context — SOUL.md, AGENTS.md read from `agent_root`
/// 4. User's custom `system_prompt` / `prompt_file`
pub fn build_system_prompt(
    agent_id: &str,
    agents: &HashMap<String, AgentConfig>,
    teams: &HashMap<String, TeamConfig>,
    agent_root: Option<&Path>,
    config_system_prompt: Option<&str>,
) -> String {
    let mut prompt = BUILTIN_AGENT_INSTRUCTIONS.to_string();

    // Build teammate block
    let start_marker = "<!-- TEAMMATES_START -->";
    let end_marker = "<!-- TEAMMATES_END -->";

    let mut teammates = vec![];
    for team in teams.values() {
        if !team.agents.iter().any(|member| member == agent_id) {
            continue;
        }
        for tid in &team.agents {
            if tid == agent_id {
                continue;
            }
            if let Some(agent) = agents.get(tid) {
                let entry = format!("- `@{}` — **{}** ({})", tid, agent.name, agent.model);
                if !teammates.contains(&entry) {
                    teammates.push(entry);
                }
            }
        }
    }

    let mut block = String::new();
    if let Some(self_agent) = agents.get(agent_id) {
        block.push_str(&format!(
            "\n### You\n\n- `@{}` — **{}** ({})\n",
            agent_id, self_agent.name, self_agent.model
        ));
    }
    if !teammates.is_empty() {
        block.push_str("\n### Your Teammates\n\n");
        teammates.sort();
        for t in &teammates {
            block.push_str(t);
            block.push('\n');
        }
    }

    // Inject teammate block
    if let (Some(start_idx), Some(end_idx)) =
        (prompt.find(start_marker), prompt.find(end_marker))
    {
        prompt = format!(
            "{}{}{}",
            &prompt[..start_idx + start_marker.len()],
            block,
            &prompt[end_idx..],
        );
    }

    // Inject workspace bootstrap files (SOUL.md, AGENTS.md, etc.)
    if let Some(root) = agent_root {
        let ws_context = inject_workspace_context(root);
        if !ws_context.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(ws_context.trim_end());
        }
    }

    // Append user's config system prompt
    if let Some(sp) = config_system_prompt {
        let sp = sp.trim();
        if !sp.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(sp);
        }
    }

    prompt
}

/// Build the "## Workspace Context" section by reading workspace bootstrap files.
///
/// Each file is read from `agent_root`, truncated at [`BOOTSTRAP_MAX_CHARS`],
/// and rendered as a `### filename` subsection. Missing or empty files are
/// silently skipped — ClawPod creates these files at workspace init, so their
/// absence simply means the user deleted them intentionally.
fn inject_workspace_context(agent_root: &Path) -> String {
    let mut section = String::new();
    for &(subdir, filename) in WORKSPACE_FILES {
        let path = if subdir.is_empty() {
            agent_root.join(filename)
        } else {
            agent_root.join(subdir).join(filename)
        };
        inject_workspace_file(&mut section, &path, filename);
    }
    if section.is_empty() {
        return section;
    }
    let mut out = String::from("## Workspace Context\n\n");
    out.push_str(&section);
    out
}

/// Read a single workspace file and append it as a `### filename` block.
///
/// - Empty files and read errors are silently skipped.
/// - Content is truncated at [`BOOTSTRAP_MAX_CHARS`] with a notice appended.
fn inject_workspace_file(prompt: &mut String, path: &Path, display_name: &str) {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return;
    }
    let _ = writeln!(prompt, "### {display_name}\n");
    let char_count = trimmed.chars().count();
    if char_count > BOOTSTRAP_MAX_CHARS {
        let truncated = trimmed
            .char_indices()
            .nth(BOOTSTRAP_MAX_CHARS)
            .map(|(idx, _)| &trimmed[..idx])
            .unwrap_or(trimmed);
        prompt.push_str(truncated);
        let _ = writeln!(
            prompt,
            "\n\n[... truncated at {BOOTSTRAP_MAX_CHARS} chars]\n"
        );
    } else {
        prompt.push_str(trimmed);
        prompt.push_str("\n\n");
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Helper: create a temp workspace with optional SOUL.md and AGENTS.md content.
    fn make_workspace(
        soul: Option<&str>,
        agents_md: Option<&str>,
    ) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let root = dir.path();
        fs::create_dir_all(root.join(".clawpod")).unwrap();
        if let Some(content) = soul {
            fs::write(root.join(".clawpod").join("SOUL.md"), content).unwrap();
        }
        if let Some(content) = agents_md {
            fs::write(root.join("AGENTS.md"), content).unwrap();
        }
        dir
    }

    #[test]
    fn inject_workspace_file_reads_and_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TEST.md");
        fs::write(&path, "  Hello world  ").unwrap();

        let mut buf = String::new();
        inject_workspace_file(&mut buf, &path, "TEST.md");

        assert!(buf.contains("### TEST.md"));
        assert!(buf.contains("Hello world"));
        // Content should be trimmed
        assert!(!buf.contains("  Hello world  "));
    }

    #[test]
    fn inject_workspace_file_skips_missing() {
        let mut buf = String::new();
        inject_workspace_file(&mut buf, Path::new("/nonexistent/file.md"), "file.md");
        assert!(buf.is_empty());
    }

    #[test]
    fn inject_workspace_file_skips_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("EMPTY.md");
        fs::write(&path, "   \n\n  ").unwrap();

        let mut buf = String::new();
        inject_workspace_file(&mut buf, &path, "EMPTY.md");
        assert!(buf.is_empty());
    }

    #[test]
    fn inject_workspace_file_truncates_large_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("BIG.md");
        // Create content exceeding BOOTSTRAP_MAX_CHARS
        let content: String = "x".repeat(BOOTSTRAP_MAX_CHARS + 500);
        fs::write(&path, &content).unwrap();

        let mut buf = String::new();
        inject_workspace_file(&mut buf, &path, "BIG.md");

        assert!(buf.contains("### BIG.md"));
        assert!(buf.contains("[... truncated at 20000 chars]"));
        // Should not contain the full content
        assert!(buf.len() < content.len());
    }

    #[test]
    fn inject_workspace_file_truncates_multibyte_at_char_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MULTI.md");
        // Each Japanese char is 3 bytes in UTF-8
        let content: String = "あ".repeat(BOOTSTRAP_MAX_CHARS + 100);
        fs::write(&path, &content).unwrap();

        let mut buf = String::new();
        inject_workspace_file(&mut buf, &path, "MULTI.md");

        assert!(buf.contains("[... truncated at 20000 chars]"));
        // Verify we didn't panic or corrupt UTF-8
        assert!(buf.is_char_boundary(buf.len()));
    }

    #[test]
    fn inject_workspace_context_combines_files() {
        let ws = make_workspace(
            Some("# TestBot\nI am a test bot."),
            Some("# Agent Workspace\nYou are @test"),
        );

        let section = inject_workspace_context(ws.path());

        assert!(section.starts_with("## Workspace Context"));
        assert!(section.contains("### SOUL.md"));
        assert!(section.contains("I am a test bot."));
        assert!(section.contains("### AGENTS.md"));
        assert!(section.contains("You are @test"));
    }

    #[test]
    fn inject_workspace_context_empty_when_no_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".clawpod")).unwrap();

        let section = inject_workspace_context(dir.path());
        assert!(section.is_empty());
    }

    #[test]
    fn inject_workspace_context_partial_files() {
        let ws = make_workspace(Some("# Soul content"), None);

        let section = inject_workspace_context(ws.path());

        assert!(section.contains("### SOUL.md"));
        assert!(section.contains("# Soul content"));
        assert!(!section.contains("### AGENTS.md"));
    }

    #[test]
    fn build_system_prompt_injects_workspace_context() {
        let ws = make_workspace(
            Some("# TestBot\nI have strong opinions about testing."),
            None,
        );

        let agents = HashMap::new();
        let teams = HashMap::new();
        let prompt = build_system_prompt("test", &agents, &teams, Some(ws.path()), None);

        assert!(prompt.contains("## Workspace Context"));
        assert!(prompt.contains("### SOUL.md"));
        assert!(prompt.contains("I have strong opinions about testing."));
    }

    #[test]
    fn build_system_prompt_works_without_agent_root() {
        let agents = HashMap::new();
        let teams = HashMap::new();
        let prompt = build_system_prompt("test", &agents, &teams, None, Some("Custom instructions"));

        assert!(prompt.contains("ClawPod"));
        assert!(prompt.contains("Custom instructions"));
        assert!(!prompt.contains("## Workspace Context"));
    }

    #[test]
    fn build_system_prompt_ordering() {
        let ws = make_workspace(Some("# Soul"), None);
        let agents = HashMap::new();
        let teams = HashMap::new();
        let prompt = build_system_prompt(
            "test",
            &agents,
            &teams,
            Some(ws.path()),
            Some("User custom prompt"),
        );

        let builtin_pos = prompt.find("ClawPod").unwrap();
        let ws_context_pos = prompt.find("## Workspace Context").unwrap();
        let user_pos = prompt.find("User custom prompt").unwrap();

        // Verify ordering: builtin < workspace context < user prompt
        assert!(
            builtin_pos < ws_context_pos,
            "builtin instructions should come before workspace context"
        );
        assert!(
            ws_context_pos < user_pos,
            "workspace context should come before user prompt"
        );
    }
}
