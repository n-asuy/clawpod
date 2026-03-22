use std::fmt::Write;
use std::fs;
use std::path::Path;

use anyhow::Result;

use crate::prompt::{PromptContext, PromptSection};

/// Directory under workspace_dir where skills live.
const SKILLS_DIR: &str = ".clawpod/skills";

/// Maximum number of skills included in the prompt.
const MAX_SKILLS_IN_PROMPT: usize = 50;

// ---------------------------------------------------------------------------
// SkillEntry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    /// Relative path from workspace root to the SKILL.md file.
    pub rel_path: String,
}

// ---------------------------------------------------------------------------
// Frontmatter parsing
// ---------------------------------------------------------------------------

/// Extract the YAML frontmatter block from a SKILL.md file.
///
/// Expects content starting with `---\n` and ending at the next `---\n`.
/// Returns the raw frontmatter text between the delimiters (without the `---` lines).
fn extract_frontmatter(content: &str) -> Option<&str> {
    let trimmed = content.trim_start();
    let rest = trimmed.strip_prefix("---")?;
    // The first `---` must be followed by a newline (or be at end of string).
    let rest = rest.strip_prefix('\n').or_else(|| rest.strip_prefix("\r\n"))?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

/// Parse a simple `key: value` from frontmatter text.
///
/// Handles quoted and unquoted values. Only single-line values are supported.
fn frontmatter_value<'a>(frontmatter: &'a str, key: &str) -> Option<&'a str> {
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(value_part) = rest.strip_prefix(':') {
                let value = value_part.trim();
                // Strip surrounding quotes if present.
                if (value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\''))
                {
                    return Some(&value[1..value.len() - 1]);
                }
                return Some(value);
            }
        }
    }
    None
}

/// Parse a SKILL.md file and extract a `SkillEntry`.
fn parse_skill_md(content: &str, dir_name: &str, rel_path: &str) -> Option<SkillEntry> {
    let fm = extract_frontmatter(content)?;
    let name = frontmatter_value(fm, "name")
        .map(|s| s.to_string())
        .unwrap_or_else(|| dir_name.to_string());
    let description = frontmatter_value(fm, "description")?.to_string();
    if description.is_empty() {
        return None;
    }
    Some(SkillEntry {
        name,
        description,
        rel_path: rel_path.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Skill discovery
// ---------------------------------------------------------------------------

/// Scan `<workspace_dir>/.clawpod/skills/` for skill directories containing SKILL.md.
pub fn load_skills(workspace_dir: &Path) -> Vec<SkillEntry> {
    let skills_dir = workspace_dir.join(SKILLS_DIR);
    let entries = match fs::read_dir(&skills_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut skills: Vec<SkillEntry> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let dir_name = entry.file_name().into_string().ok()?;
            let skill_md = entry.path().join("SKILL.md");
            let content = fs::read_to_string(&skill_md).ok()?;
            let rel_path = format!("{SKILLS_DIR}/{dir_name}/SKILL.md");
            parse_skill_md(&content, &dir_name, &rel_path)
        })
        .collect();

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills.truncate(MAX_SKILLS_IN_PROMPT);
    skills
}

// ---------------------------------------------------------------------------
// SkillsSection
// ---------------------------------------------------------------------------

pub struct SkillsSection;

impl PromptSection for SkillsSection {
    fn name(&self) -> &str {
        "skills"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let skills = load_skills(ctx.workspace_dir);
        if skills.is_empty() {
            return Ok(String::new());
        }

        let mut out = String::from("## Available Skills\n\n");
        out.push_str(
            "When a user's request matches a skill below, read its SKILL.md for detailed instructions before proceeding.\n\n",
        );

        for skill in &skills {
            let _ = writeln!(
                out,
                "- **{}**: {} (`{}`)",
                skill.name, skill.description, skill.rel_path
            );
        }

        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_skill_dir(root: &Path, name: &str, skill_md: &str) {
        let dir = root.join(SKILLS_DIR).join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), skill_md).unwrap();
    }

    fn make_ctx<'a>(
        workspace_dir: &'a Path,
        agents: &'a HashMap<String, domain::AgentConfig>,
        teams: &'a HashMap<String, domain::TeamConfig>,
    ) -> PromptContext<'a> {
        PromptContext {
            workspace_dir,
            agent_id: "test",
            agents,
            teams,
            user_system_prompt: None,
        }
    }

    // -- extract_frontmatter --

    #[test]
    fn extract_frontmatter_basic() {
        let content = "---\nname: foo\ndescription: bar\n---\n# Body";
        let fm = extract_frontmatter(content).unwrap();
        assert_eq!(fm, "name: foo\ndescription: bar");
    }

    #[test]
    fn extract_frontmatter_with_leading_whitespace() {
        let content = "  ---\nname: foo\n---\n";
        let fm = extract_frontmatter(content).unwrap();
        assert_eq!(fm, "name: foo");
    }

    #[test]
    fn extract_frontmatter_missing_opener() {
        assert!(extract_frontmatter("name: foo\n---\n").is_none());
    }

    #[test]
    fn extract_frontmatter_missing_closer() {
        assert!(extract_frontmatter("---\nname: foo\n").is_none());
    }

    // -- frontmatter_value --

    #[test]
    fn frontmatter_value_simple() {
        let fm = "name: my-skill\ndescription: does things";
        assert_eq!(frontmatter_value(fm, "name"), Some("my-skill"));
        assert_eq!(frontmatter_value(fm, "description"), Some("does things"));
    }

    #[test]
    fn frontmatter_value_quoted() {
        let fm = "name: \"my skill\"\ndescription: 'a description'";
        assert_eq!(frontmatter_value(fm, "name"), Some("my skill"));
        assert_eq!(frontmatter_value(fm, "description"), Some("a description"));
    }

    #[test]
    fn frontmatter_value_missing_key() {
        let fm = "name: foo";
        assert!(frontmatter_value(fm, "description").is_none());
    }

    // -- parse_skill_md --

    #[test]
    fn parse_skill_md_full() {
        let content = "---\nname: deploy\ndescription: Deploy the app\n---\n# Deploy\n...";
        let entry = parse_skill_md(content, "deploy", ".clawpod/skills/deploy/SKILL.md").unwrap();
        assert_eq!(entry.name, "deploy");
        assert_eq!(entry.description, "Deploy the app");
    }

    #[test]
    fn parse_skill_md_name_defaults_to_dir() {
        let content = "---\ndescription: Does things\n---\nbody";
        let entry = parse_skill_md(content, "my-dir", ".clawpod/skills/my-dir/SKILL.md").unwrap();
        assert_eq!(entry.name, "my-dir");
    }

    #[test]
    fn parse_skill_md_missing_description_returns_none() {
        let content = "---\nname: foo\n---\nbody";
        assert!(parse_skill_md(content, "foo", "path").is_none());
    }

    #[test]
    fn parse_skill_md_empty_description_returns_none() {
        let content = "---\nname: foo\ndescription:\n---\nbody";
        // description value is empty string
        assert!(parse_skill_md(content, "foo", "path").is_none());
    }

    #[test]
    fn parse_skill_md_no_frontmatter_returns_none() {
        let content = "# Just markdown\nNo frontmatter here.";
        assert!(parse_skill_md(content, "dir", "path").is_none());
    }

    // -- load_skills --

    #[test]
    fn load_skills_discovers_valid_skills() {
        let dir = tempfile::tempdir().unwrap();
        make_skill_dir(
            dir.path(),
            "alpha",
            "---\nname: alpha\ndescription: Alpha skill\n---\n# Alpha",
        );
        make_skill_dir(
            dir.path(),
            "beta",
            "---\nname: beta\ndescription: Beta skill\n---\n# Beta",
        );

        let skills = load_skills(dir.path());
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "alpha");
        assert_eq!(skills[1].name, "beta");
    }

    #[test]
    fn load_skills_skips_invalid_entries() {
        let dir = tempfile::tempdir().unwrap();
        make_skill_dir(
            dir.path(),
            "valid",
            "---\nname: valid\ndescription: A valid skill\n---\nbody",
        );
        // Missing description
        make_skill_dir(dir.path(), "bad", "---\nname: bad\n---\nbody");
        // Not a directory — just a stray file
        fs::create_dir_all(dir.path().join(SKILLS_DIR)).unwrap();
        fs::write(
            dir.path().join(SKILLS_DIR).join("stray.txt"),
            "not a skill",
        )
        .unwrap();

        let skills = load_skills(dir.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "valid");
    }

    #[test]
    fn load_skills_empty_when_no_skills_dir() {
        let dir = tempfile::tempdir().unwrap();
        let skills = load_skills(dir.path());
        assert!(skills.is_empty());
    }

    #[test]
    fn load_skills_sorted_alphabetically() {
        let dir = tempfile::tempdir().unwrap();
        make_skill_dir(
            dir.path(),
            "zeta",
            "---\nname: zeta\ndescription: Z\n---\n",
        );
        make_skill_dir(
            dir.path(),
            "alpha",
            "---\nname: alpha\ndescription: A\n---\n",
        );
        make_skill_dir(
            dir.path(),
            "mid",
            "---\nname: mid\ndescription: M\n---\n",
        );

        let skills = load_skills(dir.path());
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }

    // -- SkillsSection --

    #[test]
    fn skills_section_renders_available_skills() {
        let dir = tempfile::tempdir().unwrap();
        make_skill_dir(
            dir.path(),
            "deploy",
            "---\nname: deploy\ndescription: Deploy the application\n---\n# Deploy",
        );
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = make_ctx(dir.path(), &agents, &teams);
        let output = SkillsSection.build(&ctx).unwrap();

        assert!(output.contains("## Available Skills"));
        assert!(output.contains("**deploy**"));
        assert!(output.contains("Deploy the application"));
        assert!(output.contains("`.clawpod/skills/deploy/SKILL.md`"));
    }

    #[test]
    fn skills_section_empty_when_no_skills() {
        let dir = tempfile::tempdir().unwrap();
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = make_ctx(dir.path(), &agents, &teams);
        let output = SkillsSection.build(&ctx).unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn skills_section_multiple_skills() {
        let dir = tempfile::tempdir().unwrap();
        make_skill_dir(
            dir.path(),
            "build",
            "---\nname: build\ndescription: Build project\n---\n",
        );
        make_skill_dir(
            dir.path(),
            "test",
            "---\nname: test\ndescription: Run tests\n---\n",
        );
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = make_ctx(dir.path(), &agents, &teams);
        let output = SkillsSection.build(&ctx).unwrap();

        assert!(output.contains("**build**"));
        assert!(output.contains("**test**"));
    }
}
