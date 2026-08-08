//! Discord server-wide feature settings and channel tracking.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::engine::disambig::{disambiguate_batch, DisambigConfig};
use crate::engine::s2t::S2TConverter;
use crate::engine::scan::{ContentType, ScanOutput, Scanner};
use crate::engine::zhtype::{detect_chinese_type, ChineseType};
use crate::rules::loader::load_embedded_ruleset;
use crate::rules::ruleset::{CaseRule, Issue, IssueType, Profile, RuleType, Ruleset, SpellingRule};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Terminology,
    Spacing,
    CaseDictionary,
    CustomRules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureSet {
    #[serde(default = "default_enabled")]
    pub terminology: bool,
    #[serde(default = "default_enabled")]
    pub spacing: bool,
    #[serde(default = "default_enabled")]
    pub case_dictionary: bool,
    #[serde(default = "default_enabled")]
    pub custom_rules: bool,
}

const fn default_enabled() -> bool {
    true
}

impl Default for FeatureSet {
    fn default() -> Self {
        Self {
            terminology: true,
            spacing: true,
            case_dictionary: true,
            custom_rules: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default)]
    pub features: FeatureSet,
    #[serde(default)]
    pub custom_spelling_rules: Vec<SpellingRule>,
    #[serde(default)]
    pub custom_case_rules: Vec<CaseRule>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub tracking: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    servers: BTreeMap<u64, ServerConfig>,
    #[serde(default)]
    channels: BTreeMap<String, ChannelConfig>,
}

pub struct DiscordConfig {
    path: PathBuf,
    file: Mutex<ConfigFile>,
}

impl DiscordConfig {
    pub fn from_env() -> Result<Self> {
        let path = std::env::var_os("TWLINTER_CONFIG_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(default_config_path);
        Self::load(path)
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let file = if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("read Discord config {}", path.display()))?;
            serde_json::from_str(&content).context("parse Discord config")?
        } else {
            ConfigFile::default()
        };
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn server(&self, guild_id: u64) -> ServerConfig {
        self.file
            .lock()
            .expect("Discord config mutex is not poisoned")
            .servers
            .get(&guild_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn channel(&self, guild_id: u64, channel_id: u64) -> ChannelConfig {
        self.file
            .lock()
            .expect("Discord config mutex is not poisoned")
            .channels
            .get(&channel_key(guild_id, channel_id))
            .copied()
            .unwrap_or_default()
    }

    pub fn tracked_channels(&self, guild_id: u64) -> Vec<u64> {
        let prefix = format!("{guild_id}:");
        self.file
            .lock()
            .expect("Discord config mutex is not poisoned")
            .channels
            .iter()
            .filter_map(|(key, config)| {
                config
                    .tracking
                    .then(|| key.strip_prefix(&prefix)?.parse().ok())
                    .flatten()
            })
            .collect()
    }

    pub fn update_server(&self, guild_id: u64, server: ServerConfig) -> Result<()> {
        self.update(|file| {
            file.servers.insert(guild_id, server);
        })
    }

    pub fn update_channel(
        &self,
        guild_id: u64,
        channel_id: u64,
        channel: ChannelConfig,
    ) -> Result<()> {
        self.update(|file| {
            file.channels
                .insert(channel_key(guild_id, channel_id), channel);
        })
    }

    fn update(&self, change: impl FnOnce(&mut ConfigFile)) -> Result<()> {
        let mut file = self
            .file
            .lock()
            .expect("Discord config mutex is not poisoned");
        let mut next = file.clone();
        change(&mut next);
        persist(&self.path, &next)?;
        *file = next;
        Ok(())
    }
}

fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| Path::new(".").to_path_buf())
        .join("twlinter")
        .join("discord.json")
}

fn channel_key(guild_id: u64, channel_id: u64) -> String {
    format!("{guild_id}:{channel_id}")
}

fn persist(path: &Path, file: &ConfigFile) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create Discord config directory {}", parent.display()))?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(
        &temporary,
        format!("{}\n", serde_json::to_string_pretty(file)?),
    )
    .with_context(|| format!("write {}", temporary.display()))?;
    std::fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

pub struct DiscordLinter {
    ruleset: Ruleset,
    s2t: S2TConverter,
}

pub struct DiscordLint {
    pub normalized_text: String,
    pub input_was_simplified: bool,
    pub output: ScanOutput,
}

impl DiscordLinter {
    pub fn new() -> Result<Self> {
        Ok(Self {
            ruleset: load_embedded_ruleset()?,
            s2t: S2TConverter::new(),
        })
    }

    pub fn lint(
        &self,
        text: &str,
        server: &ServerConfig,
        channel: ChannelConfig,
    ) -> Option<DiscordLint> {
        if !channel.tracking {
            return None;
        }

        let features = server.features;
        let input_was_simplified =
            features.terminology && detect_chinese_type(text) == ChineseType::Simplified;
        let normalized_text = if input_was_simplified {
            self.s2t.convert(text)
        } else {
            text.to_owned()
        };

        let mut spelling_rules = if features.terminology {
            self.ruleset.spelling_rules.clone()
        } else {
            Vec::new()
        };
        let mut case_rules = if features.case_dictionary {
            self.ruleset.case_rules.clone()
        } else {
            Vec::new()
        };
        if features.custom_rules {
            spelling_rules.extend(server.custom_spelling_rules.iter().cloned());
            case_rules.extend(server.custom_case_rules.iter().cloned());
        }

        let scanner = Scanner::new(spelling_rules, case_rules);
        let mut config = Profile::Base.config();
        config.spelling = features.terminology || features.custom_rules;
        config.casing = features.case_dictionary || features.custom_rules;
        config.basic_punctuation = features.spacing;
        config.colon_enforcement = false;
        config.dunhao_detection = false;
        config.range_normalization = false;
        config.variant_normalization = false;
        config.ellipsis_normalization = false;
        config.grammar_checks = false;
        config.ai_filler_detection = false;
        config.translationese_detection = false;
        config.ai_semantic_safety = false;
        config.ai_density_detection = false;
        config.ai_structural_patterns = false;

        let mut output = scanner.scan_for_content_type_with_config(
            &normalized_text,
            ContentType::Markdown,
            config,
        );
        output
            .issues
            .retain(|issue| is_enabled_issue(issue, features));
        let _ = disambiguate_batch(
            &mut output.issues,
            &normalized_text,
            &DisambigConfig {
                profile: Profile::Base,
                ..Default::default()
            },
        );

        Some(DiscordLint {
            normalized_text,
            input_was_simplified,
            output,
        })
    }
}

fn is_enabled_issue(issue: &Issue, features: FeatureSet) -> bool {
    let terminology = features.terminology || features.custom_rules;
    let casing = features.case_dictionary || features.custom_rules;
    (terminology && is_terminology_issue(issue.rule_type))
        || (casing && issue.rule_type == IssueType::Case)
        || (features.spacing && is_spacing_issue(issue))
}

fn is_terminology_issue(issue_type: IssueType) -> bool {
    matches!(
        issue_type,
        IssueType::PoliticalColoring
            | IssueType::CrossStrait
            | IssueType::Typo
            | IssueType::Confusable
            | IssueType::Variant
    )
}

fn is_spacing_issue(issue: &Issue) -> bool {
    issue.rule_type == IssueType::Punctuation
        && issue
            .context
            .as_deref()
            .is_some_and(|context| context.contains("空格") || context.contains("數字應使用"))
}

pub fn feature_name(feature: Feature) -> &'static str {
    match feature {
        Feature::Terminology => "terminology",
        Feature::Spacing => "spacing",
        Feature::CaseDictionary => "case_dictionary",
        Feature::CustomRules => "custom_rules",
    }
}

pub fn invite_url(client_id: u64) -> String {
    format!(
        "https://discord.com/oauth2/authorize?client_id={client_id}&scope=bot%20applications.commands&permissions=68608"
    )
}

pub fn set_feature(features: &mut FeatureSet, name: &str, enabled: bool) -> Result<()> {
    match name {
        "terminology" => features.terminology = enabled,
        "spacing" => features.spacing = enabled,
        "case_dictionary" => features.case_dictionary = enabled,
        "custom_rules" => features.custom_rules = enabled,
        _ => anyhow::bail!("未知功能：{name}"),
    }
    Ok(())
}

pub fn spelling_rule(from: &str, to: &str) -> SpellingRule {
    SpellingRule {
        from: from.to_owned(),
        to: vec![to.to_owned()],
        rule_type: RuleType::CrossStrait,
        disabled: false,
        context: None,
        english: None,
        exceptions: None,
        context_clues: None,
        negative_context_clues: None,
        positional_clues: None,
        tags: None,
        editorial_confidence: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_only_controls_tracking() {
        let config = DiscordConfig::load("target/test-discord-config.json").unwrap();
        let server = config.server(1);
        assert!(!config.channel(1, 2).tracking);
        assert!(DiscordLinter::new()
            .unwrap()
            .lint("軟件", &server, ChannelConfig::default())
            .is_none());
        assert!(DiscordLinter::new()
            .unwrap()
            .lint("軟件", &server, ChannelConfig { tracking: true })
            .is_some());
    }

    #[test]
    fn feature_filter_is_server_wide() {
        let linter = DiscordLinter::new().unwrap();
        let server = ServerConfig {
            features: FeatureSet {
                terminology: false,
                spacing: false,
                case_dictionary: true,
                custom_rules: false,
            },
            ..Default::default()
        };
        let output = linter
            .lint("github", &server, ChannelConfig { tracking: true })
            .unwrap();
        assert!(output
            .output
            .issues
            .iter()
            .all(|i| i.rule_type == IssueType::Case));
    }

    #[test]
    fn custom_rules_are_opt_in() {
        let linter = DiscordLinter::new().unwrap();
        let server = ServerConfig {
            features: FeatureSet {
                terminology: false,
                spacing: false,
                case_dictionary: false,
                custom_rules: true,
            },
            custom_spelling_rules: vec![spelling_rule("測試詞", "測試用語")],
            ..Default::default()
        };
        let output = linter
            .lint("測試詞", &server, ChannelConfig { tracking: true })
            .unwrap();
        assert_eq!(output.output.issues.len(), 1);
        assert_eq!(output.output.issues[0].suggestions[0], "測試用語");
    }

    #[test]
    fn invite_url_uses_bot_id_and_required_scopes() {
        assert_eq!(
            invite_url(123),
            "https://discord.com/oauth2/authorize?client_id=123&scope=bot%20applications.commands&permissions=68608"
        );
    }
}
