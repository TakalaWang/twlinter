use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Linting profile controlling zh-TW norm enforcement strictness.
///
/// Two profiles on the strictness axis:
/// - Base: cross-strait vocabulary, political coloring, casing, basic
///   punctuation, grammar. No character variant normalization.
/// - Strict: full Ministry of Education enforcement including character
///   variant normalization (裏→裡, 台→臺).
///
/// Orthogonal capabilities (applied on top of any profile):
/// - `relaxed`: disables colon enforcement, dunhao detection, grammar
///   checks, and uses en-dash for ranges. For software UI strings.
/// - `detect_ai`: enables AI writing artifact detection (filler phrases,
///   semantic safety words, copula/passive checks, density patterns).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Base,
    Strict,
}

/// Processing chain configuration for a profile.
///
/// Each profile is a combination of enabled rule stages rather than a
/// subset of rules. More specific profiles (strict) add extra stages;
/// they do not replace earlier ones.
#[derive(Debug, Clone, Copy)]
pub struct ProfileConfig {
    /// Enable spelling rules (cross-strait, political, typo, confusable).
    pub spelling: bool,
    /// Enable case rules (proper noun casing).
    pub casing: bool,
    /// Enable basic punctuation: comma, period, !, ?, ;, (, ).
    pub basic_punctuation: bool,
    /// Enable full-width colon enforcement (: -> ：).
    pub colon_enforcement: bool,
    /// Enable enumeration comma (dunhao) detection.
    pub dunhao_detection: bool,
    /// Enable range indicator normalization (~, -).
    pub range_normalization: bool,
    /// Enable character variant normalization (裏->裡, 綫->線).
    pub variant_normalization: bool,
    /// Enable ellipsis normalization: ... → ……, 。。。 → …….
    pub ellipsis_normalization: bool,
    /// Range indicator style: true = en dash (–), false = wave dash (～).
    pub range_en_dash: bool,
    /// Enable grammar checks (interlingual transfer, A-not-A + 嗎 clash).
    pub grammar_checks: bool,
    /// Enable AI filler phrase detection (值得注意的是, 在這種情況下, etc.).
    pub ai_filler_detection: bool,
    /// Enable translationese (翻譯腔 / 歐化) detection — lexical patterns
    /// from the dewesternise checklist.  Orthogonal to `ai_filler_detection`:
    /// a translated manual is 歐化 but not AI-generated.
    pub translationese_detection: bool,
    /// Domain calibration for translationese scoring thresholds.
    /// `General` uses balanced thresholds; `Technical`/`Literary`/`News`
    /// shift the per-signal thresholds to match domain norms.
    pub translationese_domain: crate::engine::translationese_score::TranslationeseDomain,
    /// Enable AI semantic safety word detection (意味著 disambiguation,
    /// copula avoidance, passive voice overuse).
    pub ai_semantic_safety: bool,
    /// Enable density-based AI phrase detection.  Counts tracked phrases
    /// across the full document and flags when density exceeds per-phrase
    /// thresholds (count per thousand characters).
    pub ai_density_detection: bool,
    /// Enable structural AI pattern detection: binary contrast density,
    /// paragraph-ending declarations, dash overuse, formulaic headings.
    pub ai_structural_patterns: bool,
    /// AI detection threshold multiplier: <1.0 = sensitive (catches more),
    /// 1.0 = balanced (default), >1.0 = conservative (fewer false positives).
    /// Maps to ai_threshold levels: low=0.5, medium=1.0, high=1.5.
    pub ai_threshold_multiplier: f32,
    /// When true (default for Markdown content), boost severity by one level
    /// for issues whose span is fully contained inside a heading.  Headings
    /// are higher-visibility than body prose and warrant stricter treatment.
    pub heading_severity_boost: bool,
    /// Political stance sub-profile. Controls which PoliticalColoring rules fire.
    pub political_stance: PoliticalStance,
    /// When true, skip line/col computation (byte offsets only).
    /// Used by consumers that only need byte offsets.
    pub offset_only: bool,
    /// When true (Markdown content only), exclude pulldown-cmark
    /// `Tag::BlockQuote` ranges from scanning.  Off by default — adopted
    /// blockquote prose is real content.  Opt-in via `--exempt-blockquotes`
    /// or `[markdown] exempt_blockquotes = true` (35.7).
    pub exempt_blockquotes: bool,
}

impl ProfileConfig {
    /// Return a copy with the political stance overridden.
    pub fn with_stance(mut self, stance: PoliticalStance) -> Self {
        self.political_stance = stance;
        self
    }

    /// Apply the 'relaxed' capability: disable colon enforcement, dunhao
    /// detection, and grammar checks; use en-dash for ranges. Designed for
    /// software UI strings where strict punctuation rules are too noisy.
    pub fn with_relaxed(mut self) -> Self {
        self.colon_enforcement = false;
        self.dunhao_detection = false;
        self.grammar_checks = false;
        self.range_en_dash = true;
        self
    }

    /// Mark blockquote prose as excluded from scanning.  Useful when a
    /// document contains long mainland-Chinese citations the author
    /// cannot rewrite.
    pub fn with_exempt_blockquotes(mut self, on: bool) -> Self {
        self.exempt_blockquotes = on;
        self
    }
}

impl Profile {
    /// All defined profiles.
    pub const ALL: &'static [Profile] = &[Profile::Base, Profile::Strict];

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Profile::Base => "base",
            Profile::Strict => "strict",
        }
    }

    /// Short description.
    pub fn description(self) -> &'static str {
        match self {
            Profile::Base => "Base zh-TW rules: cross-strait vocabulary, political coloring, casing, basic punctuation, grammar",
            Profile::Strict => "Full MoE enforcement: all punctuation, character variants, 臺 normalization, grammar",
        }
    }

    /// Processing chain stages enabled by this profile.
    pub fn config(self) -> ProfileConfig {
        match self {
            Profile::Base => ProfileConfig {
                spelling: true,
                casing: true,
                basic_punctuation: true,
                colon_enforcement: true,
                dunhao_detection: true,
                range_normalization: true,
                variant_normalization: false,
                ellipsis_normalization: true,
                range_en_dash: false,
                grammar_checks: true,
                ai_filler_detection: true,
                translationese_detection: true,
                ai_semantic_safety: false,
                ai_density_detection: false,
                ai_structural_patterns: false,
                ai_threshold_multiplier: 1.0,
                translationese_domain:
                    crate::engine::translationese_score::TranslationeseDomain::General,
                heading_severity_boost: true,
                political_stance: PoliticalStance::RocCentric,
                offset_only: false,
                exempt_blockquotes: false,
            },
            Profile::Strict => ProfileConfig {
                spelling: true,
                casing: true,
                basic_punctuation: true,
                colon_enforcement: true,
                dunhao_detection: true,
                range_normalization: true,
                variant_normalization: true,
                ellipsis_normalization: true,
                range_en_dash: false,
                grammar_checks: true,
                ai_filler_detection: true,
                translationese_detection: true,
                ai_semantic_safety: false,
                ai_density_detection: false,
                ai_structural_patterns: false,
                ai_threshold_multiplier: 1.0,
                translationese_domain:
                    crate::engine::translationese_score::TranslationeseDomain::General,
                heading_severity_boost: true,
                political_stance: PoliticalStance::RocCentric,
                offset_only: false,
                exempt_blockquotes: false,
            },
        }
    }

    /// Strict parse from string. Returns `None` on unrecognized input.
    pub fn from_str_strict(s: &str) -> Option<Self> {
        match s {
            "base" => Some(Profile::Base),
            "strict" => Some(Profile::Strict),
            _ => None,
        }
    }
}

/// Political stance sub-profile controlling which PoliticalColoring rules fire.
///
/// Orthogonal to the main Profile enum. When None (or RocCentric), all
/// political_coloring rules apply (current default). International keeps only
/// organization/entity name normalization (東盟→東協). Neutral suppresses all
/// political_coloring rules entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoliticalStance {
    /// Apply all political_coloring rules (default behavior).
    RocCentric,
    /// Only apply organization/entity name rules; skip identity-loaded terms
    /// (內地, 祖國, 大陸同胞).
    International,
    /// Suppress all political_coloring rules.
    Neutral,
}

impl PoliticalStance {
    /// All defined stances.
    pub const ALL: &'static [PoliticalStance] = &[
        PoliticalStance::RocCentric,
        PoliticalStance::International,
        PoliticalStance::Neutral,
    ];

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            PoliticalStance::RocCentric => "roc_centric",
            PoliticalStance::International => "international",
            PoliticalStance::Neutral => "neutral",
        }
    }

    /// Short description.
    pub fn description(self) -> &'static str {
        match self {
            PoliticalStance::RocCentric => {
                "Apply all political/regional normalization rules (default)"
            }
            PoliticalStance::International => {
                "Only normalize international organization names (東盟→東協); skip identity terms"
            }
            PoliticalStance::Neutral => "Suppress all political coloring rules",
        }
    }

    /// Parse from string, defaulting to RocCentric on unrecognized input.
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "international" => PoliticalStance::International,
            "neutral" => PoliticalStance::Neutral,
            _ => PoliticalStance::RocCentric,
        }
    }

    /// Strict parse from string. Returns `None` on unrecognized input.
    pub fn from_str_strict(s: &str) -> Option<Self> {
        match s {
            "roc_centric" => Some(PoliticalStance::RocCentric),
            "international" => Some(PoliticalStance::International),
            "neutral" => Some(PoliticalStance::Neutral),
            _ => None,
        }
    }

    /// Whether a specific political_coloring rule should fire under this stance.
    ///
    /// Identity-loaded terms (內地, 大陸同胞, 祖國) are suppressed under
    /// International. All terms suppressed under Neutral.
    pub fn allows_rule(self, from: &str) -> bool {
        match self {
            PoliticalStance::RocCentric => true,
            PoliticalStance::Neutral => false,
            PoliticalStance::International => {
                // Suppress identity-loaded terms; keep organization names.
                !matches!(from, "內地" | "大陸同胞" | "祖國")
            }
        }
    }
}

/// Tier 2 disambiguation outcome stored on each Issue.
/// Used by Tier 3 (external LLM decisions) to determine eligibility without
/// fragile context-string parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tier2Outcome {
    /// Not processed by Tier 2 (deterministic types, no english/clues).
    #[default]
    NotEligible,
    /// Resolved locally by Tier 2 (hard anchor, collocation, clue, prior).
    Resolved,
    /// Suppressed by Tier 2 (score below ambiguous threshold).
    Suppressed,
    /// Gray zone — eligible for a Tier 3 LLM judgment.
    GrayZone,
}

/// Which tier authored the resolution of an issue.
/// Injected per-issue in JSON output when `include_stats` is true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionTier {
    /// Pure rule match — no disambiguation needed (punctuation, case,
    /// variant, grammar, unambiguous spelling).
    Deterministic,
    /// Resolved by Tier 2 local heuristics (context clues, profile
    /// priors, collocations, combined evidence).
    Heuristic,
    /// Resolved by a Tier 3 LLM decision.
    LlmJudged,
    /// Not conclusively resolved: suppressed as likely FP, skipped
    /// by budget exhaustion, or left in gray zone without LLM.
    Unresolved,
}

impl ResolutionTier {
    /// Derive the resolution tier from the issue's tier2_outcome and
    /// whether an LLM produced a judgment (indicated by context
    /// annotation).
    pub fn classify(issue: &Issue) -> Self {
        match issue.tier2_outcome {
            Tier2Outcome::NotEligible => ResolutionTier::Deterministic,
            Tier2Outcome::Resolved => ResolutionTier::Heuristic,
            Tier2Outcome::Suppressed => ResolutionTier::Unresolved,
            Tier2Outcome::GrayZone => {
                if issue.llm_judged {
                    ResolutionTier::LlmJudged
                } else {
                    ResolutionTier::Unresolved
                }
            }
        }
    }
}

/// Rule types for spelling/terminology rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleType {
    /// Mainland China political coloring
    PoliticalColoring,
    /// Cross-strait usage difference
    CrossStrait,
    /// Typo / spelling correction
    Typo,
    /// Confusable term
    Confusable,
    /// Character variant: MoE standard form differs from non-standard glyph
    /// (e.g. 裏->裡, 綫->線). Curated from OpenCC TWVariants.txt.
    Variant,
    /// AI filler phrase: zero-information hedging/emphasis inserted by LLMs.
    /// Fixed-string AC matches; deletions or simple substitutions.
    AiFiller,
    /// Translationese (翻譯腔 / 歐化): Europeanized Chinese syntax and
    /// vocabulary.  Orthogonal to AI detection — a translated manual is
    /// 歐化 but not AI-generated.  Sourced from the dewesternise checklist
    /// (余光中《論中文之西化》 and related literature).
    Translationese,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    /// Human-readable lowercase name.
    pub fn name(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }

    /// Single-letter severity for compact/grep-style output.
    pub fn letter(self) -> &'static str {
        match self {
            Severity::Info => "I",
            Severity::Warning => "W",
            Severity::Error => "E",
        }
    }
}

impl RuleType {
    pub fn default_severity(self) -> Severity {
        match self {
            RuleType::PoliticalColoring | RuleType::Typo => Severity::Error,
            RuleType::CrossStrait | RuleType::Confusable | RuleType::Variant => Severity::Warning,
            RuleType::AiFiller | RuleType::Translationese => Severity::Info,
        }
    }
}

/// A spelling/terminology rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpellingRule {
    /// The term to match (source form to be flagged).
    pub from: String,
    /// One or more replacement suggestions (target forms).
    pub to: Vec<String>,
    /// Classification of this rule.
    #[serde(rename = "type")]
    pub rule_type: RuleType,
    /// If true, this rule is disabled and will not be used for scanning.
    #[serde(default)]
    pub disabled: bool,
    /// Usage context that helps the AI agent pick the right suggestion
    /// when multiple correct forms exist or when the term is ambiguous.
    #[serde(default)]
    pub context: Option<String>,
    /// English original term — serves as an unambiguous anchor when the
    /// same Chinese term means different things across the strait.
    /// E.g. 並行 = concurrency (TW) vs parallelism (CN).
    #[serde(default)]
    pub english: Option<String>,
    /// Exception phrases where the matched form should not be flagged.
    /// Applies to all rule types (variant, cross_strait, typo, confusable).
    /// E.g. chess term 下著 keeps 着; 分類 keeps 類 from firing as a class
    /// warning.  An empty or absent list means no exceptions.
    #[serde(default)]
    pub exceptions: Option<Vec<String>>,
    /// Surrounding words that suggest the intended meaning for ambiguous terms.
    /// When present, the fixer uses segmentation to check if these clue words
    /// appear near the match. E.g. 程序 with clues ["編寫", "代碼", "執行"]
    /// suggests the "program" sense rather than "procedure".
    #[serde(default)]
    pub context_clues: Option<Vec<String>>,
    /// Words that, when present in the surrounding window, indicate the term is
    /// being used correctly in context and should NOT be flagged.  Acts as a
    /// veto: if any negative clue matches, the rule is skipped regardless of
    /// positive context_clues.  E.g. 項目 should not fire when 的 or 等
    /// precede it (list-item grammatical usage vs. project/IT usage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_context_clues: Option<Vec<String>>,
    /// Positional conditions that constrain WHERE a context term must appear
    /// relative to the match.  More expressive than flat context_clues (which
    /// check presence anywhere in +-40-char window).  Syntax:
    ///
    /// - `before:TERM` — TERM must appear within 20 chars AFTER the match
    /// - `after:TERM` — TERM must appear within 20 chars BEFORE the match
    /// - `adjacent:TERM` — TERM must be immediately adjacent (no gap)
    /// - `not_before:TERM` — TERM must NOT appear within 20 chars after
    /// - `not_after:TERM` — TERM must NOT appear within 20 chars before
    ///
    /// All positive conditions must pass (AND). Any negative condition vetoes.
    /// When both context_clues and positional_clues are present, both must
    /// match (AND).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub positional_clues: Option<Vec<String>>,
    /// Optional tags for categorization and filtering in rule packs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Per-rule editorial confidence (35.2).  Distinguishes binary
    /// corrections from style-preference suggestions.  When set to
    /// `Low`, the explain pipeline marks issues from this rule as
    /// `auto_fix_safe = false` AND `needs_review = true` — these are
    /// terms whose Mainland/Taiwan distinction is genuine but where
    /// the calque form is also valid zh-TW vocabulary in some senses
    /// (e.g. `優化`, `算法`, `場景`).  Defaults to `None` (heuristic
    /// derivation in `derive_explain_meta`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editorial_confidence: Option<EditorialConfidence>,
}

/// Editorial confidence tier surfaced in explain output (35.2).
///
/// Per-issue field that distinguishes binary corrections (`線程` →
/// `執行緒`, high) from editorial-judgment terms (`優化` is valid
/// zh-TW; `最佳化` is preferred in formal writing — low).  Distinct
/// from `summary_metrics.confidence_distribution`, which tracks
/// resolution-tier confidence across the document.
///
/// Invariant enforced downstream: `Low` ⇒ `auto_fix_safe = false`
/// AND `needs_review = true`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EditorialConfidence {
    High,
    Medium,
    Low,
}

impl SpellingRule {
    /// True when this rule is an AiFiller deletion (`to: [""]`): the matched
    /// phrase should be removed entirely, with the empty string as the fix.
    pub fn is_deletion_rule(&self) -> bool {
        self.rule_type == RuleType::AiFiller && self.to.len() == 1 && self.to[0].is_empty()
    }

    /// Create a spelling rule with required fields; optional fields default to None.
    #[cfg(test)]
    pub fn new(from: impl Into<String>, to: Vec<String>, rule_type: RuleType) -> Self {
        Self {
            from: from.into(),
            to,
            rule_type,
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
}

/// A proper noun casing rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseRule {
    /// The canonical correct casing (e.g. "JavaScript").
    pub term: String,
    /// Other accepted casings (e.g. ["javascript", "JAVASCRIPT"]).
    #[serde(default)]
    pub alternatives: Option<Vec<String>>,
    /// If true, this rule is disabled and will not be used for scanning.
    #[serde(default)]
    pub disabled: bool,
}

/// Top-level ruleset container — the JSON source format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ruleset {
    pub spelling_rules: Vec<SpellingRule>,
    pub case_rules: Vec<CaseRule>,
}

/// An issue found by the scanner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Byte offset in the original text.
    pub offset: usize,
    /// Byte length of the matched span.
    pub length: usize,
    /// 1-based line number in the original text.
    pub line: usize,
    /// 1-based column number (UTF-16 code units by default, matching LSP spec).
    pub col: usize,
    /// The matched (wrong) text.
    pub found: String,
    /// Suggested replacements.  Arc avoids per-issue allocation during
    /// inflate — most issues share suggestions with their source rule.
    pub suggestions: Arc<[String]>,
    /// Manual rewrite hint for translationese issues.  Derived from the
    /// first non-empty suggestion when a translationese rule supplies one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_rewrite: Option<String>,
    /// Classification of the triggering rule.
    pub rule_type: IssueType,
    /// Severity level.
    pub severity: Severity,
    /// Usage context from the triggering rule, helping the AI agent
    /// choose the right suggestion or understand the nuance.
    /// Arc-interned during inflation to avoid per-issue String clones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Arc<str>>,
    /// English original term — unambiguous anchor for cross-strait terms.
    /// Arc-interned during inflation to avoid per-issue String clones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub english: Option<Arc<str>>,
    /// Context clues from the triggering rule. Fixer uses these with a
    /// segmenter to decide whether an ambiguous term should be corrected.
    /// Arc-interned during inflation to avoid per-issue Vec clones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_clues: Option<Arc<[String]>>,
    /// Calibration result from translation verification.
    /// `Some(true)`: anchor found in translation (confirmed).
    /// `Some(false)`: anchor absent in translation (unconfirmed).
    /// `None`: calibration not attempted or API failure (no signal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_match: Option<bool>,
    /// Internal flag for project-glossary banned-term precedence.
    /// When true, TM must not downgrade the issue, but the marker
    /// stays out of user-facing `context` metadata.
    #[serde(skip)]
    pub glossary_banned: bool,
    /// Tier 2 disambiguation outcome.  Set by `disambiguate_batch` to
    /// indicate whether the issue was resolved locally, suppressed, or
    /// left in the gray zone for Tier 3.  Internal — not serialized.
    #[serde(skip)]
    pub tier2_outcome: Tier2Outcome,
    /// Whether Tier 3 LLM produced a judgment for this issue.
    /// Set by the external decision adapter.
    /// Used by `ResolutionTier::classify` to distinguish LLM-judged from
    /// unresolved gray-zone issues without fragile string parsing.
    #[serde(skip)]
    pub llm_judged: bool,
    /// Internal: deferred spelling rule index for lazy issue inflation.
    /// When Some, suggestions/context/english/context_clues are empty
    /// placeholders that must be inflated from the compiled DB after
    /// overlap resolution.
    #[serde(skip)]
    pub(crate) spelling_rule_idx: Option<usize>,
    /// Markdown table cell coordinates `(row, column)` (0-based) when the
    /// issue falls inside a Markdown table cell.  Useful for editor
    /// integrations and SARIF region output.  `None` when not in a table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_cell: Option<TableCell>,
    /// Per-issue editorial confidence (35.2).  Copied from the source
    /// `SpellingRule` during inflation; surfaces in explain output
    /// via `derive_explain_meta`.  `None` means heuristic derivation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editorial_confidence: Option<EditorialConfidence>,
}

/// Markdown table cell coordinates: `(row, column)` are 0-based, with row 0
/// being the header row (or row 1 if the table has no separator).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TableCell {
    pub row: usize,
    pub col: usize,
}

impl Issue {
    /// Derive the manual rewrite hint: the first non-empty suggestion, but
    /// only for translationese rules (other rule types have no rewrite hint).
    pub fn derive_suggested_rewrite(
        rule_type: IssueType,
        suggestions: &[String],
    ) -> Option<String> {
        if rule_type == IssueType::Translationese {
            suggestions.iter().find(|s| !s.is_empty()).cloned()
        } else {
            None
        }
    }

    pub fn refresh_suggested_rewrite(&mut self) {
        self.suggested_rewrite = Self::derive_suggested_rewrite(self.rule_type, &self.suggestions);
    }

    /// Construct an issue with all semantic fields; line/col are set to 0
    /// (filled in later by the line-index pass).
    pub fn new(
        offset: usize,
        length: usize,
        found: impl Into<String>,
        suggestions: Vec<String>,
        rule_type: IssueType,
        severity: Severity,
    ) -> Self {
        let suggested_rewrite = Self::derive_suggested_rewrite(rule_type, &suggestions);
        Self {
            offset,
            length,
            line: 0,
            col: 0,
            found: found.into(),
            suggestions: suggestions.into(),
            suggested_rewrite,
            rule_type,
            severity,
            context: None,
            english: None,
            context_clues: None,
            anchor_match: None,
            glossary_banned: false,
            tier2_outcome: Tier2Outcome::NotEligible,
            llm_judged: false,
            spelling_rule_idx: None,
            table_cell: None,
            editorial_confidence: None,
        }
    }

    /// Lightweight constructor for deferred spelling issues.
    ///
    /// Skips the `found` and `suggestions` allocations — those are filled
    /// during inflation after overlap resolution.  Uses a static empty
    /// Arc to avoid per-issue heap allocation.
    pub(crate) fn deferred_spelling(
        offset: usize,
        length: usize,
        rule_type: IssueType,
        severity: Severity,
        rule_idx: usize,
    ) -> Self {
        static EMPTY_SUGGESTIONS: std::sync::OnceLock<Arc<[String]>> = std::sync::OnceLock::new();
        Self {
            offset,
            length,
            line: 0,
            col: 0,
            found: String::new(),
            suggestions: EMPTY_SUGGESTIONS
                .get_or_init(|| Arc::from(Vec::<String>::new()))
                .clone(),
            suggested_rewrite: None,
            rule_type,
            severity,
            context: None,
            english: None,
            context_clues: None,
            anchor_match: None,
            glossary_banned: false,
            tier2_outcome: Tier2Outcome::NotEligible,
            llm_judged: false,
            spelling_rule_idx: Some(rule_idx),
            table_cell: None,
            editorial_confidence: None,
        }
    }

    /// Builder: attach context string.
    pub fn with_context(mut self, ctx: impl Into<String>) -> Self {
        self.context = Some(Arc::from(ctx.into()));
        self
    }

    /// Builder: attach english anchor.
    pub fn with_english(mut self, eng: impl Into<String>) -> Self {
        self.english = Some(Arc::from(eng.into()));
        self
    }

    /// Builder: attach context clues.
    pub fn with_context_clues(mut self, clues: Vec<String>) -> Self {
        self.context_clues = Some(Arc::from(clues));
        self
    }
}

/// Issue classification — covers spelling, case, punctuation, grammar, and AI style checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    PoliticalColoring,
    CrossStrait,
    Typo,
    Confusable,
    Case,
    Punctuation,
    Variant,
    Grammar,
    /// AI writing artifact: filler phrases, semantic safety words, copula
    /// avoidance, passive voice overuse.  NOT eligible for orthographic-tier
    /// fixes — requires lexical_contextual or none.
    AiStyle,
    /// Consecutive duplicate word or character (e.g. '去去', 'cache cache').
    Repetition,
    /// Translationese (翻譯腔 / 歐化): Europeanized Chinese syntax/vocabulary.
    /// Orthogonal to AiStyle — separate score and output surface.
    Translationese,
}

impl IssueType {
    /// Stable ordering key for deterministic output (used by scan sort).
    pub fn sort_order(self) -> u8 {
        match self {
            IssueType::PoliticalColoring => 0,
            IssueType::CrossStrait => 1,
            IssueType::Typo => 2,
            IssueType::Confusable => 3,
            IssueType::Case => 4,
            IssueType::Punctuation => 5,
            IssueType::Variant => 6,
            IssueType::Grammar => 7,
            IssueType::AiStyle => 8,
            IssueType::Repetition => 9,
            IssueType::Translationese => 10,
        }
    }

    /// Snake_case name matching serde serialization.
    pub fn name(self) -> &'static str {
        match self {
            IssueType::PoliticalColoring => "political_coloring",
            IssueType::CrossStrait => "cross_strait",
            IssueType::Typo => "typo",
            IssueType::Confusable => "confusable",
            IssueType::Case => "case",
            IssueType::Punctuation => "punctuation",
            IssueType::Variant => "variant",
            IssueType::Grammar => "grammar",
            IssueType::AiStyle => "ai_style",
            IssueType::Repetition => "repetition",
            IssueType::Translationese => "translationese",
        }
    }
}

/// Text shown for a rule that deletes its match rather than replacing it.
pub const DELETE_SUGGESTION: &str = "(delete)";

/// True when a suggestion list means "delete the match": exactly one entry,
/// and that entry empty. Every output format has to special-case this, so the
/// predicate lives next to the issue rather than in each formatter.
pub fn is_delete_suggestion(suggestions: &[String]) -> bool {
    suggestions.len() == 1 && suggestions[0].is_empty()
}

impl Issue {
    /// Compact suggestion string: first suggestion only, `+N` suffix for alternatives.
    /// Falls back to `english` field when no suggestions exist.
    pub fn compact_suggestion(&self) -> String {
        if self.suggestions.is_empty() {
            self.english.as_deref().unwrap_or("?").to_string()
        } else if is_delete_suggestion(&self.suggestions) {
            DELETE_SUGGESTION.to_string()
        } else if self.suggestions.len() == 1 {
            self.suggestions[0].clone()
        } else {
            format!("{}+{}", self.suggestions[0], self.suggestions.len() - 1)
        }
    }

    /// Grouping key for deduplication in compact output.
    /// Issues with identical (found, rule_type, suggestions, severity) are collapsible.
    /// Uses full suggestion list (joined) rather than compact display form to avoid
    /// merging issues with different alternative sets.
    pub fn compact_dedup_key(&self) -> (&str, &'static str, String, &'static str) {
        (
            &self.found,
            self.rule_type.name(),
            self.suggestions.join("|"),
            self.severity.letter(),
        )
    }
}

impl From<RuleType> for IssueType {
    fn from(rt: RuleType) -> Self {
        match rt {
            RuleType::PoliticalColoring => IssueType::PoliticalColoring,
            RuleType::CrossStrait => IssueType::CrossStrait,
            RuleType::Translationese => IssueType::Translationese,
            RuleType::Typo => IssueType::Typo,
            RuleType::Confusable => IssueType::Confusable,
            RuleType::Variant => IssueType::Variant,
            RuleType::AiFiller => IssueType::AiStyle,
        }
    }
}

#[cfg(test)]
mod delete_suggestion_tests {
    use super::*;

    fn sugs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn only_a_lone_empty_suggestion_means_delete() {
        assert!(is_delete_suggestion(&sugs(&[""])));
        // An empty list means "no suggestion", not "delete".
        assert!(!is_delete_suggestion(&[]));
        assert!(!is_delete_suggestion(&sugs(&["軟體"])));
        // Two entries, one empty, is an alternatives list.
        assert!(!is_delete_suggestion(&sugs(&["", "軟體"])));
    }

    #[test]
    fn compact_suggestion_renders_the_delete_sentinel() {
        let issue = Issue::new(
            0,
            3,
            "\u{200B}",
            sugs(&[""]),
            IssueType::AiStyle,
            Severity::Info,
        );
        assert_eq!(issue.compact_suggestion(), DELETE_SUGGESTION);
    }
}
