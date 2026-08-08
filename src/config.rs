// Project config file support (.zhtw-core.toml).
//
// Discovery: resolve once from cwd upward, stopping at VCS root (.git) or
// filesystem root.  Apply globally to all files in the run.
// CLI flags override config file values.  Config file overrides defaults.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Config file name.
const CONFIG_FILENAME: &str = ".zhtw-core.toml";

/// Parsed project config.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub profile: Option<String>,
    pub relaxed: Option<bool>,
    pub content_type: Option<String>,
    pub max_errors: Option<usize>,
    pub max_warnings: Option<usize>,
    pub ignore_terms: Option<Vec<String>>,
    pub exclude: Option<Vec<String>>,
    pub overrides: Option<String>,
    pub suppressions: Option<String>,
    pub packs: Option<Vec<String>>,
    pub translation_memory: Option<String>,
    pub markdown: Option<MarkdownConfig>,
    pub glossary: Option<GlossaryConfig>,
}

/// Markdown-specific scanning options (35.7).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct MarkdownConfig {
    /// When true, treat pulldown-cmark `Tag::BlockQuote` ranges as
    /// exclusion zones.  Useful for documents that quote mainland-Chinese
    /// sources for illustrative purposes.  Off by default.
    pub exempt_blockquotes: Option<bool>,
}

/// Project glossary section (35.9).  Layered above the embedded ruleset
/// and pack store but below banned-term enforcement and translation
/// memory.  Precedence: glossary `banned` > TM > glossary `preferred` >
/// domain pack > embedded ruleset.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GlossaryConfig {
    /// Terms that must always be flagged regardless of context clues.
    /// E.g. ["線程", "內存"] forces those calques to fire even in
    /// otherwise ambiguous prose.
    pub banned: Option<Vec<String>>,
    /// Project-preferred zh-TW forms.  Used by the consistency report
    /// (35.1) to choose the canonical suggestion when both TW-preferred
    /// and CN-preferred variants appear in the same document.
    pub preferred: Option<Vec<String>>,
    /// Names that should never be flagged (added to the suppression
    /// list).  E.g. ["TSMC", "MediaTek"].
    pub proper_nouns: Option<Vec<String>>,
}

impl ProjectConfig {
    /// Discover and parse the nearest .zhtw-core.toml.
    ///
    /// Walks from start_dir upward, stopping at a .git directory or
    /// filesystem root.  Returns None if no config file is found.
    pub fn discover(start_dir: &Path) -> Option<Self> {
        let path = find_config_file(start_dir)?;
        let content = std::fs::read_to_string(&path).ok()?;
        match toml::from_str::<ProjectConfig>(&content) {
            Ok(cfg) => {
                tracing::info!("loaded config from {}", path.display());
                Some(cfg)
            }
            Err(e) => {
                tracing::warn!("failed to parse {}: {}", path.display(), e);
                None
            }
        }
    }

    /// Load from an explicit path (--config flag).
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read config {}: {}", path.display(), e))?;
        toml::from_str::<ProjectConfig>(&content)
            .map_err(|e| anyhow::anyhow!("parse config {}: {}", path.display(), e))
    }
}

/// Walk from start upward looking for .zhtw-core.toml.
/// Stop at .git directory or filesystem root.
fn find_config_file(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(CONFIG_FILENAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        // Stop at VCS root.
        if dir.join(".git").exists() {
            return None;
        }
        // Move to parent.
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn discover_finds_config_in_cwd() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_FILENAME),
            "profile = \"strict\"\nmax_errors = 5\n",
        )
        .unwrap();
        let cfg = ProjectConfig::discover(dir.path()).unwrap();
        assert_eq!(cfg.profile.as_deref(), Some("strict"));
        assert_eq!(cfg.max_errors, Some(5));
    }

    #[test]
    fn discover_walks_upward() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("sub").join("deep");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.path().join(CONFIG_FILENAME), "profile = \"base\"\n").unwrap();
        let cfg = ProjectConfig::discover(&sub).unwrap();
        assert_eq!(cfg.profile.as_deref(), Some("base"));
    }

    #[test]
    fn discover_stops_at_git_root() {
        let dir = TempDir::new().unwrap();
        // Place config above .git boundary.
        std::fs::write(dir.path().join(CONFIG_FILENAME), "profile = \"base\"\n").unwrap();
        let sub = dir.path().join("repo");
        std::fs::create_dir_all(sub.join(".git")).unwrap();
        let deep = sub.join("src");
        std::fs::create_dir_all(&deep).unwrap();
        // Discovery from deep should not find the config above .git.
        let cfg = ProjectConfig::discover(&deep);
        assert!(cfg.is_none());
    }

    #[test]
    fn discover_returns_none_when_absent() {
        let dir = TempDir::new().unwrap();
        // Create .git so we stop quickly.
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert!(ProjectConfig::discover(dir.path()).is_none());
    }

    #[test]
    fn parse_all_fields() {
        let toml = r#"
profile = "strict"
content_type = "markdown"
max_errors = 0
max_warnings = 10
ignore_terms = ["軟件", "硬件"]
exclude = ["vendor/**", "*.tmp"]
overrides = "/path/to/overrides.json"
suppressions = "/path/to/suppressions.json"
packs = ["medical", "legal"]
"#;
        let cfg: ProjectConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.profile.as_deref(), Some("strict"));
        assert_eq!(cfg.content_type.as_deref(), Some("markdown"));
        assert_eq!(cfg.max_errors, Some(0));
        assert_eq!(cfg.max_warnings, Some(10));
        assert_eq!(cfg.ignore_terms.as_ref().unwrap().len(), 2);
        assert_eq!(cfg.exclude.as_ref().unwrap().len(), 2);
        assert_eq!(cfg.overrides.as_deref(), Some("/path/to/overrides.json"));
        assert_eq!(cfg.packs.as_ref().unwrap(), &["medical", "legal"]);
    }

    #[test]
    fn parse_empty_config() {
        let cfg: ProjectConfig = toml::from_str("").unwrap();
        assert!(cfg.profile.is_none());
        assert!(cfg.max_errors.is_none());
    }
}
