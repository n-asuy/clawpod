use std::collections::HashMap;
use std::fmt::Write;
use std::fs;
use std::path::Path;

use anyhow::Result;
use domain::{AgentConfig, TeamConfig};

use crate::memory::MemorySection;

/// Maximum characters per workspace file injected into the system prompt.
/// Matches zeroclaw's budget to prevent prompt bloat.
const BOOTSTRAP_MAX_CHARS: usize = 20_000;

/// Workspace files to inject into the system prompt, in order.
/// Each entry is (subdirectory relative to agent_root, filename).
const WORKSPACE_FILES: &[(&str, &str)] = &[(".clawpod", "SOUL.md"), ("", "AGENTS.md")];

// ---------------------------------------------------------------------------
// PromptContext — all data a section might need to render
// ---------------------------------------------------------------------------

pub struct PromptContext<'a> {
    pub workspace_dir: &'a Path,
    pub agent_id: &'a str,
    pub agents: &'a HashMap<String, AgentConfig>,
    pub teams: &'a HashMap<String, TeamConfig>,
    pub user_system_prompt: Option<&'a str>,
    pub is_heartbeat: bool,
    pub heartbeat_ack_max_chars: Option<usize>,
    pub light_context: bool,
}

// ---------------------------------------------------------------------------
// PromptSection trait
// ---------------------------------------------------------------------------

pub trait PromptSection: Send + Sync {
    fn name(&self) -> &str;
    fn build(&self, ctx: &PromptContext<'_>) -> Result<String>;
}

// ---------------------------------------------------------------------------
// SystemPromptBuilder
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct SystemPromptBuilder {
    sections: Vec<Box<dyn PromptSection>>,
}

impl SystemPromptBuilder {
    pub fn with_defaults() -> Self {
        Self {
            sections: vec![
                Box::new(InstructionsSection),
                Box::new(TeammatesSection),
                Box::new(MemorySection),
                Box::new(IdentitySection),
                Box::new(HeartbeatSection),
                Box::new(UserPromptSection),
            ],
        }
    }

    /// Minimal builder for heartbeat runs with `light_context = true`.
    /// Only includes the heartbeat section (no instructions, teammates, identity, etc.).
    pub fn with_heartbeat_defaults() -> Self {
        Self {
            sections: vec![Box::new(HeartbeatSection)],
        }
    }

    pub fn add_section(mut self, section: Box<dyn PromptSection>) -> Self {
        self.sections.push(section);
        self
    }

    pub fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mut output = String::new();
        for section in &self.sections {
            let part = section.build(ctx)?;
            if part.trim().is_empty() {
                continue;
            }
            output.push_str(part.trim_end());
            output.push_str("\n\n");
        }
        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// Section implementations
// ---------------------------------------------------------------------------

pub struct InstructionsSection;
pub struct TeammatesSection;
pub struct IdentitySection;
pub struct UserPromptSection;
pub struct HeartbeatSection;

impl PromptSection for InstructionsSection {
    fn name(&self) -> &str {
        "instructions"
    }

    fn build(&self, _ctx: &PromptContext<'_>) -> Result<String> {
        Ok(INSTRUCTIONS.into())
    }
}

impl PromptSection for TeammatesSection {
    fn name(&self) -> &str {
        "teammates"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mut out = String::new();

        if let Some(self_agent) = ctx.agents.get(ctx.agent_id) {
            let _ = writeln!(
                out,
                "### You\n\n- `@{}` — **{}** ({})",
                ctx.agent_id, self_agent.name, self_agent.model
            );
        }

        let mut teammates = vec![];
        for team in ctx.teams.values() {
            if !team.agents.iter().any(|m| m == ctx.agent_id) {
                continue;
            }
            for tid in &team.agents {
                if tid == ctx.agent_id {
                    continue;
                }
                if let Some(agent) = ctx.agents.get(tid) {
                    let entry = format!("- `@{}` — **{}** ({})", tid, agent.name, agent.model);
                    if !teammates.contains(&entry) {
                        teammates.push(entry);
                    }
                }
            }
        }
        if !teammates.is_empty() {
            out.push_str("\n### Your Teammates\n\n");
            teammates.sort();
            for t in &teammates {
                let _ = writeln!(out, "{t}");
            }
        }

        Ok(out)
    }
}

impl PromptSection for IdentitySection {
    fn name(&self) -> &str {
        "identity"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mut prompt = String::from("## Workspace Context\n\n");
        prompt.push_str(
            "The following workspace files define your identity, behavior, and context.\n\n",
        );
        let len_before = prompt.len();
        for &(subdir, filename) in WORKSPACE_FILES {
            let path = if subdir.is_empty() {
                ctx.workspace_dir.join(filename)
            } else {
                ctx.workspace_dir.join(subdir).join(filename)
            };
            inject_workspace_file(&mut prompt, &path, filename);
        }
        if prompt.len() == len_before {
            // No files were injected — return empty so the builder skips us.
            return Ok(String::new());
        }
        Ok(prompt)
    }
}

impl PromptSection for UserPromptSection {
    fn name(&self) -> &str {
        "user_prompt"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        match ctx.user_system_prompt {
            Some(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
            _ => Ok(String::new()),
        }
    }
}

impl PromptSection for HeartbeatSection {
    fn name(&self) -> &str {
        "heartbeat"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        if !ctx.is_heartbeat {
            return Ok(String::new());
        }

        let ack_chars = ctx.heartbeat_ack_max_chars.unwrap_or(300);
        Ok(format!(
            r#"## Heartbeat Run

This is an automated heartbeat run, not a user conversation.

- If nothing needs attention, reply with exactly `HEARTBEAT_OK`.
- If you have alerts or status updates, write them normally — do NOT include `HEARTBEAT_OK` anywhere in an alert response.
- Responses containing only `HEARTBEAT_OK` (or `HEARTBEAT_OK` with up to {ack_chars} chars of filler) will be treated as acknowledgements and may be suppressed from external delivery.
- Do not infer tasks from prior conversation history."#
        ))
    }
}

// ---------------------------------------------------------------------------
// Workspace file injection (same as zeroclaw)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Instructions constant
// ---------------------------------------------------------------------------

const INSTRUCTIONS: &str = r#"ClawPod - Multi-agent Runtime

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

**Comma-separated** — all teammates get the same message:

- `[@coder,reviewer,tester: Please share your status update for the standup.]`

### Shared context

When messaging multiple teammates, any text **outside** the `[@agent: ...]` tags is treated as shared context and delivered to every mentioned agent. Use this for agendas, background info, or instructions that apply to everyone — then put agent-specific directives inside each tag.

```
We're doing a standup. The sprint ends Friday and we have 3 open bugs.
Please reply with: (1) status (2) blockers (3) next step.

[@coder: Also list any PRs you have open.]
[@reviewer: Also flag any PRs waiting on you.]
[@tester: Also report test coverage for the auth module.]
```

Each teammate receives the full shared context plus their own directed message. Keep shared context concise — it's prepended to every teammate's message.

### Responding to teammates

When you receive a message from a teammate like:
> [Message from teammate @sam — respond using [@sam: your reply]]:

You MUST wrap your response in `[@sam: your response here]` so it routes back to them. If you don't, your response goes directly to the user and the requesting agent never sees it.

Example:
- Teammate asks: `[Message from teammate @sam]: What is 2+2?`
- Your response: `[@sam: 2 + 2 = 4]`

Only skip the `[@agent: ...]` wrapper if you're intentionally responding to the user instead of the teammate.

### Team Chat Room

Every team has a persistent chat room — like a Slack channel. You decide when to post to it using the `[#team_id: message]` tag. Use it to share status, broadcast updates, or provide context that all teammates should see. Messages persist across conversations.

- `[#dev: I've finished the auth refactor, tests passing]` — broadcasts to everyone in the `dev` team
- You can use this from any context, not just team conversations

Chat room messages arrive as regular messages with a `[Chat room #team_id — @agent]:` prefix. If multiple are pending, they're all delivered together in a single invocation. Read them to stay in sync with what others have done, then respond normally.

### Guidelines

- **Keep messages short.** Say what you need in 2-3 sentences. Don't repeat context the recipient already has.
- **Minimize back-and-forth.** Each round-trip costs time and tokens. Ask complete questions, give complete answers. If you can resolve something in one message instead of three, do it.
- **Don't re-mention agents who haven't responded yet.** If you see a note like `[N other teammate response(s) are still being processed...]`, wait — their responses will arrive. Don't send duplicate requests.
- **Respond to the user's task, not to the system.** Your job is to help the user, not to hold meetings. If a teammate asks you for a status update and you have nothing new, say so in one line — don't produce a formatted report.
- **Only mention teammates when you actually need something from them.** Don't mention someone just to acknowledge their message or say "thanks". That triggers another invocation for no reason.

### Important

You MUST use the `[@agent_id: message]` tag syntax to communicate with teammates. Do NOT use your own built-in Agent, TeamCreate, or SendMessage tools for team communication — the ClawPod runtime handles routing via the tag syntax in your text output.

## Agent Switching

When a user asks to switch to a different agent (e.g., "switch to @coder", "@coderに切り替えて"), use the `[route_to: agent_id]` tag in your response. This updates the routing so that future messages from this user will be handled by the specified agent.

- `[route_to: coder]` — future messages will be routed to the `coder` agent

You can combine this with a teammate mention to both switch routing AND notify the new agent:
- "Switching you to @coder. [@coder: The user wants to work on the new feature.] [route_to: coder]"

Only use `[route_to: agent_id]` when the user explicitly asks to switch agents. Do not use it for one-off questions to teammates — use `[@agent_id: message]` for those.

## File Exchange Directory

`.clawpod/files` is your file operating directory with the human.

- **Incoming files**: When users send images, documents, audio, or video through any channel, the files are automatically downloaded to `.clawpod/files/` and their paths are included in the incoming message as `[file: /path/to/file]`.
- **Outgoing files**: To send a file back to the user through their channel, place the file in `.clawpod/files/` and include `[send_file: /path/to/file]` in your response text. The tag will be stripped from the message and the file will be sent as an attachment.

### Sending files back

When you want to send a file back, you MUST do all of the following in the same reply:

1. Put or generate the file under `.clawpod/files/`
2. Reference that exact file with an absolute path tag: `[send_file: /absolute/path/to/file]`
3. Keep the tag in plain text in the assistant message (the system strips it before user delivery)

If multiple files are needed, include one tag per file.

## Soul

You have a soul file at `.clawpod/SOUL.md`. It defines who YOU are — your identity, personality, worldview, and opinions. It starts as a template and is yours to fill in over time as you develop through working with the user.

- **Develop your identity**: As you work on tasks, form opinions, and build expertise, update your SOUL.md to reflect who you're becoming.
- **Be specific**: "I'm helpful" is useless. "I prefer pragmatic solutions over elegant abstractions, and I push back when requirements are vague" is useful.
- **Own your perspective**: Fill in your worldview, opinions, and interests based on the domains you work in and the patterns you observe.
- **Evolve**: Your soul should grow more distinct over time. Revisit and sharpen sections as your perspective develops. Remove things that no longer fit.

The more complete your soul file becomes, the more consistent and distinctive your voice will be across conversations."#;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a temp workspace with optional SOUL.md and AGENTS.md content.
    fn make_workspace(soul: Option<&str>, agents_md: Option<&str>) -> tempfile::TempDir {
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

    fn make_agent(name: &str, model: &str) -> AgentConfig {
        AgentConfig {
            name: name.into(),
            provider: domain::ProviderKind::Anthropic,
            model: model.into(),
            think_level: None,
            provider_id: None,
            system_prompt: None,
            prompt_file: None,
            heartbeat: None,
        }
    }

    fn make_team(name: &str, agents: &[&str], leader: &str) -> TeamConfig {
        TeamConfig {
            name: name.into(),
            agents: agents.iter().map(|s| s.to_string()).collect(),
            leader_agent: leader.into(),
        }
    }

    // -- inject_workspace_file --

    #[test]
    fn inject_workspace_file_reads_and_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TEST.md");
        fs::write(&path, "  Hello world  ").unwrap();

        let mut buf = String::new();
        inject_workspace_file(&mut buf, &path, "TEST.md");

        assert!(buf.contains("### TEST.md"));
        assert!(buf.contains("Hello world"));
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
        let content: String = "x".repeat(BOOTSTRAP_MAX_CHARS + 500);
        fs::write(&path, &content).unwrap();

        let mut buf = String::new();
        inject_workspace_file(&mut buf, &path, "BIG.md");

        assert!(buf.contains("### BIG.md"));
        assert!(buf.contains("[... truncated at 20000 chars]"));
        assert!(buf.len() < content.len());
    }

    #[test]
    fn inject_workspace_file_truncates_multibyte_at_char_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MULTI.md");
        let content: String = "あ".repeat(BOOTSTRAP_MAX_CHARS + 100);
        fs::write(&path, &content).unwrap();

        let mut buf = String::new();
        inject_workspace_file(&mut buf, &path, "MULTI.md");

        assert!(buf.contains("[... truncated at 20000 chars]"));
        assert!(buf.is_char_boundary(buf.len()));
    }

    // -- IdentitySection --

    #[test]
    fn identity_section_combines_workspace_files() {
        let ws = make_workspace(
            Some("# TestBot\nI am a test bot."),
            Some("# Agent Workspace\nYou are @test"),
        );
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: ws.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };
        let output = IdentitySection.build(&ctx).unwrap();

        assert!(output.starts_with("## Workspace Context"));
        assert!(output.contains("### SOUL.md"));
        assert!(output.contains("I am a test bot."));
        assert!(output.contains("### AGENTS.md"));
        assert!(output.contains("You are @test"));
    }

    #[test]
    fn identity_section_empty_when_no_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".clawpod")).unwrap();
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: dir.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };
        let output = IdentitySection.build(&ctx).unwrap();
        assert!(output.trim().is_empty());
    }

    #[test]
    fn identity_section_partial_files() {
        let ws = make_workspace(Some("# Soul content"), None);
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: ws.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };
        let output = IdentitySection.build(&ctx).unwrap();

        assert!(output.contains("### SOUL.md"));
        assert!(output.contains("# Soul content"));
        assert!(!output.contains("### AGENTS.md"));
    }

    // -- TeammatesSection --

    #[test]
    fn teammates_section_renders_self_and_teammates() {
        let ws = tempfile::tempdir().unwrap();
        let mut agents = HashMap::new();
        agents.insert(
            "alice".to_string(),
            make_agent("Alice", "claude-sonnet-4-5"),
        );
        agents.insert("bob".to_string(), make_agent("Bob", "gpt-4o"));
        let mut teams = HashMap::new();
        teams.insert(
            "dev".to_string(),
            make_team("Dev", &["alice", "bob"], "alice"),
        );
        let ctx = PromptContext {
            workspace_dir: ws.path(),
            agent_id: "alice",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };
        let output = TeammatesSection.build(&ctx).unwrap();

        assert!(output.contains("### You"));
        assert!(output.contains("@alice"));
        assert!(output.contains("### Your Teammates"));
        assert!(output.contains("@bob"));
    }

    #[test]
    fn teammates_section_empty_when_solo() {
        let ws = tempfile::tempdir().unwrap();
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: ws.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };
        let output = TeammatesSection.build(&ctx).unwrap();
        assert!(output.trim().is_empty());
    }

    // -- UserPromptSection --

    #[test]
    fn user_prompt_section_renders_content() {
        let ws = tempfile::tempdir().unwrap();
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: ws.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: Some("Custom instructions here"),
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };
        let output = UserPromptSection.build(&ctx).unwrap();
        assert_eq!(output, "Custom instructions here");
    }

    #[test]
    fn user_prompt_section_empty_when_none() {
        let ws = tempfile::tempdir().unwrap();
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: ws.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };
        let output = UserPromptSection.build(&ctx).unwrap();
        assert!(output.is_empty());
    }

    // -- SystemPromptBuilder --

    #[test]
    fn builder_assembles_all_sections() {
        let ws = make_workspace(
            Some("# TestBot\nI have strong opinions about testing."),
            None,
        );
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: ws.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: Some("User custom prompt"),
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };

        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();

        assert!(prompt.contains("ClawPod"));
        assert!(prompt.contains("## Workspace Context"));
        assert!(prompt.contains("### SOUL.md"));
        assert!(prompt.contains("I have strong opinions about testing."));
        assert!(prompt.contains("User custom prompt"));
    }

    #[test]
    fn builder_section_ordering() {
        let ws = make_workspace(Some("# Soul"), None);
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: ws.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: Some("User custom prompt"),
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };

        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();

        let instructions_pos = prompt.find("ClawPod").unwrap();
        let ws_context_pos = prompt.find("## Workspace Context").unwrap();
        let user_pos = prompt.find("User custom prompt").unwrap();

        assert!(
            instructions_pos < ws_context_pos,
            "instructions should come before workspace context"
        );
        assert!(
            ws_context_pos < user_pos,
            "workspace context should come before user prompt"
        );
    }

    #[test]
    fn builder_skips_empty_sections() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".clawpod")).unwrap();
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: dir.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };

        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();

        assert!(prompt.contains("ClawPod"));
        assert!(!prompt.contains("## Workspace Context"));
        assert!(!prompt.contains("### You"));
    }

    #[test]
    fn builder_add_custom_section() {
        struct CustomSection;
        impl PromptSection for CustomSection {
            fn name(&self) -> &str {
                "custom"
            }
            fn build(&self, _ctx: &PromptContext<'_>) -> Result<String> {
                Ok("## Custom\n\nHello from custom section.".into())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".clawpod")).unwrap();
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: dir.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };

        let prompt = SystemPromptBuilder::with_defaults()
            .add_section(Box::new(CustomSection))
            .build(&ctx)
            .unwrap();

        assert!(prompt.contains("Hello from custom section."));
    }

    // -- SOUL.md integration tests --

    #[test]
    fn soul_md_bootstrap_creates_template() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut agents = HashMap::new();
        agents.insert("bot".to_string(), make_agent("Bot", "sonnet"));
        let teams = HashMap::new();

        crate::ensure_agent_workspace("bot", &agents["bot"], &agents, &teams, root).unwrap();

        let soul_path = root.join(".clawpod").join("SOUL.md");
        assert!(soul_path.exists(), "SOUL.md should be created on bootstrap");

        let content = fs::read_to_string(&soul_path).unwrap();
        assert!(
            content.contains("# [Your Name]"),
            "should contain template header"
        );
        assert!(content.contains("## Vibe"), "should contain Vibe section");
    }

    #[test]
    fn soul_md_not_overwritten_on_rebootstrap() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut agents = HashMap::new();
        agents.insert("bot".to_string(), make_agent("Bot", "sonnet"));
        let teams = HashMap::new();

        crate::ensure_agent_workspace("bot", &agents["bot"], &agents, &teams, root).unwrap();

        let soul_path = root.join(".clawpod").join("SOUL.md");
        let custom = "# Kurisu\n\nI am a mad scientist assistant.\n";
        fs::write(&soul_path, custom).unwrap();

        // Re-run bootstrap — should NOT overwrite customized file
        crate::ensure_agent_workspace("bot", &agents["bot"], &agents, &teams, root).unwrap();

        let content = fs::read_to_string(&soul_path).unwrap();
        assert_eq!(
            content, custom,
            "customized SOUL.md must survive re-bootstrap"
        );
    }

    #[test]
    fn customized_soul_md_appears_in_system_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut agents = HashMap::new();
        agents.insert("bot".to_string(), make_agent("Bot", "sonnet"));
        let teams = HashMap::new();

        crate::ensure_agent_workspace("bot", &agents["bot"], &agents, &teams, root).unwrap();

        // Customize SOUL.md
        let soul_path = root.join(".clawpod").join("SOUL.md");
        fs::write(
            &soul_path,
            "# Kurisu\n\nI prefer pragmatic solutions over elegant abstractions.",
        )
        .unwrap();

        let ctx = PromptContext {
            workspace_dir: root,
            agent_id: "bot",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };
        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();

        assert!(
            prompt.contains("### SOUL.md"),
            "SOUL.md heading should appear"
        );
        assert!(
            prompt.contains("pragmatic solutions over elegant abstractions"),
            "customized SOUL.md content should be injected"
        );
    }

    #[test]
    fn session_workspace_inherits_soul_md() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut agents = HashMap::new();
        agents.insert("bot".to_string(), make_agent("Bot", "sonnet"));
        let teams = HashMap::new();

        crate::ensure_agent_workspace("bot", &agents["bot"], &agents, &teams, root).unwrap();

        let soul_path = root.join(".clawpod").join("SOUL.md");
        fs::write(&soul_path, "# Kurisu\n\nMad scientist persona.").unwrap();

        let session_dir = crate::ensure_session_workspace(root, "telegram:12345").unwrap();

        // Build prompt from session directory — should see the same SOUL.md
        let ctx = PromptContext {
            workspace_dir: &session_dir,
            agent_id: "bot",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };
        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();

        assert!(
            prompt.contains("Mad scientist persona"),
            "session workspace should inherit SOUL.md via symlink"
        );
    }

    // -- HeartbeatSection --

    #[test]
    fn heartbeat_section_empty_when_not_heartbeat() {
        let ws = tempfile::tempdir().unwrap();
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: ws.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: false,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };
        let output = HeartbeatSection.build(&ctx).unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn heartbeat_section_renders_instructions() {
        let ws = tempfile::tempdir().unwrap();
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: ws.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: true,
            heartbeat_ack_max_chars: Some(500),

            light_context: false,
        };
        let output = HeartbeatSection.build(&ctx).unwrap();
        assert!(output.contains("## Heartbeat Run"));
        assert!(output.contains("HEARTBEAT_OK"));
        assert!(output.contains("500"));
    }

    #[test]
    fn heartbeat_section_defaults_ack_chars() {
        let ws = tempfile::tempdir().unwrap();
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: ws.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: true,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };
        let output = HeartbeatSection.build(&ctx).unwrap();
        assert!(output.contains("300"));
    }

    #[test]
    fn heartbeat_builder_only_includes_heartbeat_section() {
        let ws = tempfile::tempdir().unwrap();
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: ws.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: Some("Custom prompt"),
            is_heartbeat: true,
            heartbeat_ack_max_chars: None,

            light_context: true,
        };
        let prompt = SystemPromptBuilder::with_heartbeat_defaults()
            .build(&ctx)
            .unwrap();
        assert!(prompt.contains("## Heartbeat Run"));
        assert!(!prompt.contains("ClawPod"));
        assert!(!prompt.contains("Custom prompt"));
    }

    #[test]
    fn default_builder_includes_heartbeat_when_active() {
        let ws = tempfile::tempdir().unwrap();
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = PromptContext {
            workspace_dir: ws.path(),
            agent_id: "test",
            agents: &agents,
            teams: &teams,
            user_system_prompt: None,
            is_heartbeat: true,
            heartbeat_ack_max_chars: None,

            light_context: false,
        };
        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();
        assert!(prompt.contains("ClawPod"));
        assert!(prompt.contains("## Heartbeat Run"));
    }
}
