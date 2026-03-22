use std::fmt::Write;
use std::fs;
use std::path::Path;

use anyhow::Result;

use crate::prompt::{PromptContext, PromptSection};

// ---------------------------------------------------------------------------
// MemoryEntry / MemoryFolder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub name: String,
    pub summary: String,
    /// Relative path from memory root to the .md file.
    pub file_path: String,
}

#[derive(Debug, Clone)]
pub struct MemoryFolder {
    pub name: String,
    /// Relative path from memory root.
    pub path: String,
    pub entries: Vec<MemoryEntry>,
    pub subfolders: Vec<MemoryFolder>,
}

// ---------------------------------------------------------------------------
// Frontmatter parsing
// ---------------------------------------------------------------------------

/// Extract the YAML frontmatter block from a markdown file.
///
/// Expects content starting with `---\n` and ending at the next `---\n`.
/// Returns the raw frontmatter text between the delimiters.
fn extract_frontmatter(content: &str) -> Option<&str> {
    let trimmed = content.trim_start();
    let rest = trimmed.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n').or_else(|| rest.strip_prefix("\r\n"))?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

/// Parse a simple `key: value` from frontmatter text.
fn frontmatter_value<'a>(frontmatter: &'a str, key: &str) -> Option<&'a str> {
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(value_part) = rest.strip_prefix(':') {
                let value = value_part.trim();
                if value.is_empty() {
                    return None;
                }
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

/// Parse a memory .md file and extract a `MemoryEntry`.
fn parse_memory_md(content: &str, rel_path: &str) -> Option<MemoryEntry> {
    let fm = extract_frontmatter(content)?;
    let name = frontmatter_value(fm, "name")?.to_string();
    let summary = frontmatter_value(fm, "summary")?.to_string();
    if name.is_empty() || summary.is_empty() {
        return None;
    }
    Some(MemoryEntry {
        name,
        summary,
        file_path: rel_path.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Recursive directory scanning
// ---------------------------------------------------------------------------

/// Recursively scan a memory directory and build the hierarchy.
/// Only reads frontmatter (name + summary), not the full content.
fn scan_memory_dir(dir_path: &Path, relative_path: &str) -> MemoryFolder {
    let folder_name = dir_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("memory")
        .to_string();

    let mut folder = MemoryFolder {
        name: folder_name,
        path: relative_path.to_string(),
        entries: Vec::new(),
        subfolders: Vec::new(),
    };

    let entries = match fs::read_dir(dir_path) {
        Ok(e) => e,
        Err(_) => return folder,
    };

    let mut items: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    items.sort_by_key(|e| e.file_name());

    for item in items {
        let name = match item.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if name.starts_with('.') {
            continue;
        }

        let item_path = item.path();
        let item_relative = if relative_path.is_empty() {
            name.clone()
        } else {
            format!("{relative_path}/{name}")
        };

        let file_type = match item.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            let subfolder = scan_memory_dir(&item_path, &item_relative);
            if !subfolder.entries.is_empty() || !subfolder.subfolders.is_empty() {
                folder.subfolders.push(subfolder);
            }
        } else if name.ends_with(".md") {
            if let Ok(content) = fs::read_to_string(&item_path) {
                if let Some(entry) = parse_memory_md(&content, &item_relative) {
                    folder.entries.push(entry);
                }
            }
        }
    }

    folder
}

/// Format a memory folder hierarchy as a readable markdown tree.
fn format_memory_tree(folder: &MemoryFolder, indent: usize) -> String {
    let mut lines = Vec::new();
    let prefix = "  ".repeat(indent);

    for entry in &folder.entries {
        lines.push(format!(
            "{prefix}- **{}** — {}  `{}`",
            entry.name, entry.summary, entry.file_path
        ));
    }

    for sub in &folder.subfolders {
        lines.push(format!("{prefix}- **[{}/]**", sub.name));
        let sub_content = format_memory_tree(sub, indent + 1);
        if !sub_content.is_empty() {
            lines.push(sub_content);
        }
    }

    lines.join("\n")
}

/// Load the memory index for an agent directory.
///
/// Returns a formatted markdown string with the hierarchical memory index,
/// or empty string if no memories exist.
pub fn load_memory_index(workspace_dir: &Path) -> String {
    let memory_dir = workspace_dir.join("memory");
    if !memory_dir.exists() {
        return String::new();
    }

    let root = scan_memory_dir(&memory_dir, "");
    if root.entries.is_empty() && root.subfolders.is_empty() {
        return String::new();
    }

    format_memory_tree(&root, 0)
}

// ---------------------------------------------------------------------------
// MemorySection
// ---------------------------------------------------------------------------

pub struct MemorySection;

impl PromptSection for MemorySection {
    fn name(&self) -> &str {
        "memory"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let tree = load_memory_index(ctx.workspace_dir);
        let mut out = String::from("## Memory\n\n");
        out.push_str(
            "Your persistent hierarchical memory. This index shows all remembered knowledge (name + summary only). To read a memory's full content, open the file at `memory/<path>`.\n\n",
        );

        if tree.is_empty() {
            out.push_str("No memories yet.\n");
        } else {
            out.push_str(&tree);
            let _ = writeln!(
                out,
                "\n\nTo read a memory in detail, read the file at `memory/<path>`."
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

    fn make_memory_file(root: &Path, rel_path: &str, content: &str) {
        let full = root.join("memory").join(rel_path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, content).unwrap();
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
        let content = "---\nname: foo\nsummary: bar\n---\n# Body";
        let fm = extract_frontmatter(content).unwrap();
        assert_eq!(fm, "name: foo\nsummary: bar");
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
        let fm = "name: my-memory\nsummary: does things";
        assert_eq!(frontmatter_value(fm, "name"), Some("my-memory"));
        assert_eq!(frontmatter_value(fm, "summary"), Some("does things"));
    }

    #[test]
    fn frontmatter_value_quoted() {
        let fm = "name: \"my memory\"\nsummary: 'a summary'";
        assert_eq!(frontmatter_value(fm, "name"), Some("my memory"));
        assert_eq!(frontmatter_value(fm, "summary"), Some("a summary"));
    }

    #[test]
    fn frontmatter_value_missing_key() {
        let fm = "name: foo";
        assert!(frontmatter_value(fm, "summary").is_none());
    }

    #[test]
    fn frontmatter_value_empty_returns_none() {
        let fm = "name:\nsummary: ok";
        assert!(frontmatter_value(fm, "name").is_none());
    }

    // -- parse_memory_md --

    #[test]
    fn parse_memory_md_full() {
        let content = "---\nname: project-goals\nsummary: Current sprint goals\n---\n# Goals\n...";
        let entry = parse_memory_md(content, "project-goals.md").unwrap();
        assert_eq!(entry.name, "project-goals");
        assert_eq!(entry.summary, "Current sprint goals");
        assert_eq!(entry.file_path, "project-goals.md");
    }

    #[test]
    fn parse_memory_md_missing_summary_returns_none() {
        let content = "---\nname: foo\n---\nbody";
        assert!(parse_memory_md(content, "foo.md").is_none());
    }

    #[test]
    fn parse_memory_md_no_frontmatter_returns_none() {
        let content = "# Just markdown\nNo frontmatter.";
        assert!(parse_memory_md(content, "foo.md").is_none());
    }

    // -- load_memory_index --

    #[test]
    fn load_memory_index_discovers_entries() {
        let dir = tempfile::tempdir().unwrap();
        make_memory_file(
            dir.path(),
            "goals.md",
            "---\nname: goals\nsummary: Sprint goals\n---\n# Goals",
        );
        make_memory_file(
            dir.path(),
            "notes.md",
            "---\nname: notes\nsummary: Meeting notes\n---\n# Notes",
        );

        let index = load_memory_index(dir.path());
        assert!(index.contains("**goals**"));
        assert!(index.contains("Sprint goals"));
        assert!(index.contains("**notes**"));
        assert!(index.contains("Meeting notes"));
    }

    #[test]
    fn load_memory_index_hierarchical() {
        let dir = tempfile::tempdir().unwrap();
        make_memory_file(
            dir.path(),
            "projects/alpha.md",
            "---\nname: alpha\nsummary: Project Alpha\n---\n",
        );
        make_memory_file(
            dir.path(),
            "projects/beta.md",
            "---\nname: beta\nsummary: Project Beta\n---\n",
        );
        make_memory_file(
            dir.path(),
            "top-level.md",
            "---\nname: top\nsummary: Top level\n---\n",
        );

        let index = load_memory_index(dir.path());
        assert!(index.contains("**[projects/]**"));
        assert!(index.contains("**alpha**"));
        assert!(index.contains("**beta**"));
        assert!(index.contains("**top**"));
    }

    #[test]
    fn load_memory_index_skips_hidden_files() {
        let dir = tempfile::tempdir().unwrap();
        make_memory_file(
            dir.path(),
            ".hidden.md",
            "---\nname: hidden\nsummary: Should not appear\n---\n",
        );
        make_memory_file(
            dir.path(),
            "visible.md",
            "---\nname: visible\nsummary: Should appear\n---\n",
        );

        let index = load_memory_index(dir.path());
        assert!(!index.contains("hidden"));
        assert!(index.contains("visible"));
    }

    #[test]
    fn load_memory_index_skips_invalid_entries() {
        let dir = tempfile::tempdir().unwrap();
        make_memory_file(
            dir.path(),
            "valid.md",
            "---\nname: valid\nsummary: A valid memory\n---\n",
        );
        // Missing summary
        make_memory_file(dir.path(), "bad.md", "---\nname: bad\n---\nbody");
        // Not markdown
        let full = dir.path().join("memory").join("stray.txt");
        fs::write(full, "not a memory").unwrap();

        let index = load_memory_index(dir.path());
        assert!(index.contains("**valid**"));
        assert!(!index.contains("bad"));
        assert!(!index.contains("stray"));
    }

    #[test]
    fn load_memory_index_empty_when_no_dir() {
        let dir = tempfile::tempdir().unwrap();
        let index = load_memory_index(dir.path());
        assert!(index.is_empty());
    }

    #[test]
    fn load_memory_index_empty_when_no_valid_entries() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("memory")).unwrap();
        let index = load_memory_index(dir.path());
        assert!(index.is_empty());
    }

    #[test]
    fn load_memory_index_skips_empty_subfolders() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("memory").join("empty-folder")).unwrap();
        make_memory_file(
            dir.path(),
            "real.md",
            "---\nname: real\nsummary: Real entry\n---\n",
        );

        let index = load_memory_index(dir.path());
        assert!(!index.contains("empty-folder"));
        assert!(index.contains("**real**"));
    }

    // -- MemorySection --

    #[test]
    fn memory_section_renders_index() {
        let dir = tempfile::tempdir().unwrap();
        make_memory_file(
            dir.path(),
            "goals.md",
            "---\nname: goals\nsummary: Sprint goals\n---\n",
        );

        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = make_ctx(dir.path(), &agents, &teams);
        let output = MemorySection.build(&ctx).unwrap();

        assert!(output.contains("## Memory"));
        assert!(output.contains("**goals**"));
        assert!(output.contains("Sprint goals"));
        assert!(output.contains("read the file at `memory/<path>`"));
    }

    #[test]
    fn memory_section_shows_no_memories() {
        let dir = tempfile::tempdir().unwrap();
        let agents = HashMap::new();
        let teams = HashMap::new();
        let ctx = make_ctx(dir.path(), &agents, &teams);
        let output = MemorySection.build(&ctx).unwrap();

        assert!(output.contains("## Memory"));
        assert!(output.contains("No memories yet."));
    }
}
