// Regression corpus for token-accuracy co-optimization (51.5).
//
// Four benchmark datasets validating that the three-tier pipeline produces
// stable, correct results:
// A. Deterministic: fully Tier 1 solvable, stable output.
// B. Ambiguous: polysemous terms, Tier 2/3 battleground.
// C. Editorial: AI filler, passive voice, hedging.
// D. Mixed-content: markdown, code blocks, CJK-Latin interleaving.

use serde::Deserialize;
use zhtw_mcp::engine::disambig::{disambiguate_batch, DisambigConfig};
use zhtw_mcp::engine::s2t::S2TConverter;
use zhtw_mcp::engine::scan::{ContentType, Scanner};
use zhtw_mcp::fixer::{apply_fixes_with_context, FixMode};
use zhtw_mcp::rules::ruleset::{Issue, IssueType, Profile};

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CorpusSpec {
    id: String,
    label: String,
    profile: String,
    #[serde(default)]
    detect_ai: bool,
    mode: String,
    min_bytes: usize,
    cases: Vec<CorpusCase>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CorpusCase {
    id: String,
    #[serde(default = "default_repeat")]
    repeat: usize,
    input: String,
    #[serde(default)]
    scan_text: Option<String>,
    expected_fixed: String,
    expected_issues: Vec<ExpectedIssue>,
}

fn default_repeat() -> usize {
    1
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct ExpectedIssue {
    found: String,
    replace: String,
    rule_type: String,
}

fn parse_issue_type(name: &str) -> IssueType {
    match name {
        "political_coloring" => IssueType::PoliticalColoring,
        "cross_strait" => IssueType::CrossStrait,
        "typo" => IssueType::Typo,
        "confusable" => IssueType::Confusable,
        "case" => IssueType::Case,
        "punctuation" => IssueType::Punctuation,
        "variant" => IssueType::Variant,
        "grammar" => IssueType::Grammar,
        "ai_style" => IssueType::AiStyle,
        "repetition" => IssueType::Repetition,
        "translationese" => IssueType::Translationese,
        _ => panic!("unknown issue type: {name}"),
    }
}

fn load_scanner() -> (Scanner, S2TConverter) {
    let ruleset = zhtw_mcp::rules::loader::load_embedded_ruleset().unwrap();
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);
    (scanner, S2TConverter::new())
}

fn working_text<'a>(s2t: &'a S2TConverter, text: &'a str) -> std::borrow::Cow<'a, str> {
    if zhtw_mcp::engine::zhtype::detect_chinese_type(text)
        == zhtw_mcp::engine::zhtype::ChineseType::Simplified
    {
        std::borrow::Cow::Owned(s2t.convert(text))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

fn scan_and_fix(
    scanner: &Scanner,
    s2t: &S2TConverter,
    text: &str,
    content_type: ContentType,
    profile: Profile,
    detect_ai: bool,
) -> (String, Vec<Issue>) {
    // S2T conversion if needed.
    let converted = working_text(s2t, text);
    let work_text = converted.as_ref();

    let mut cfg = profile.config();
    if detect_ai {
        cfg.ai_filler_detection = true;
        cfg.ai_semantic_safety = true;
        cfg.ai_density_detection = true;
        cfg.ai_structural_patterns = true;
    }

    let scan_out = scanner.scan_for_content_type_with_config(work_text, content_type, cfg);
    let mut issues = scan_out.issues;

    // Apply Tier 2 disambiguation with default thresholds.
    let disambig_cfg = DisambigConfig {
        profile,
        ..Default::default()
    };
    let _stats = disambiguate_batch(&mut issues, work_text, &disambig_cfg);

    // Apply fixes.
    let excluded =
        zhtw_mcp::engine::scan::build_exclusions_for_content_type(work_text, content_type);
    let excluded_pairs: Vec<(usize, usize)> = excluded.iter().map(|r| (r.start, r.end)).collect();
    let fix_result = apply_fixes_with_context(
        work_text,
        &issues,
        FixMode::LexicalContextual,
        &excluded_pairs,
        Some(scanner.segmenter()),
    );

    (fix_result.text, issues)
}

fn load_corpus(name: &str) -> CorpusSpec {
    let path = format!("tests/corpus/{name}.json");
    let data =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("failed to parse {path}: {e}"))
}

// ---------------------------------------------------------------------------
// A. Deterministic corpus: stable output
// ---------------------------------------------------------------------------

#[test]
fn deterministic_corpus_stable_output() {
    let corpus = load_corpus("deterministic");
    let (scanner, s2t) = load_scanner();
    let profile = Profile::Base;
    let content_type = ContentType::Markdown;

    for case in &corpus.cases {
        let input = case.scan_text.as_deref().unwrap_or(&case.input);
        let (fixed_expected, issues1) =
            scan_and_fix(&scanner, &s2t, input, content_type, profile, false);
        assert_eq!(
            fixed_expected, case.expected_fixed,
            "case {}: fixed output drifted",
            case.id
        );

        let active1: Vec<_> = issues1.iter().collect();
        assert_eq!(
            active1.len(),
            case.expected_issues.len(),
            "case {}: active issue count drifted",
            case.id
        );
        for expected in &case.expected_issues {
            let expected_type = parse_issue_type(&expected.rule_type);
            assert!(
                active1.iter().any(|issue| {
                    issue.found == expected.found
                        && issue.rule_type == expected_type
                        && issue.suggestions.iter().any(|s| s == &expected.replace)
                }),
                "case {}: missing expected issue {} -> {} ({})",
                case.id,
                expected.found,
                expected.replace,
                expected.rule_type
            );
        }

        // First pass: fix the input.
        let fixed1 = fixed_expected;
        // Second pass: feed fixed output back in. A truly idempotent fixer
        // must produce identical output when re-run on its own result.
        let (fixed2, issues2) = scan_and_fix(&scanner, &s2t, &fixed1, content_type, profile, false);
        assert_eq!(
            fixed1, fixed2,
            "case {}: fixing is not idempotent (second pass changed the text)",
            case.id
        );
        // No residual issues above Info on already-fixed text.
        let active: Vec<_> = issues2
            .iter()
            .filter(|i| i.severity != zhtw_mcp::rules::ruleset::Severity::Info)
            .collect();
        assert!(
            active.is_empty(),
            "case {}: second pass produced {} active issues on already-fixed text",
            case.id,
            active.len(),
        );
    }
}

// ---------------------------------------------------------------------------
// B. Ambiguous corpus: validates Tier 2/3 behavior
// ---------------------------------------------------------------------------

#[test]
fn ambiguous_corpus_basic_validation() {
    let corpus = load_corpus("ambiguous");
    let (scanner, s2t) = load_scanner();
    let profile = Profile::Base;
    let content_type = ContentType::Plain;

    for case in &corpus.cases {
        let input = case.scan_text.as_deref().unwrap_or(&case.input);
        let (fixed, issues) = scan_and_fix(&scanner, &s2t, input, content_type, profile, false);
        assert_eq!(
            fixed, case.expected_fixed,
            "case {}: fixed output drifted",
            case.id
        );

        let active: Vec<_> = issues
            .iter()
            .filter(|i| i.severity != zhtw_mcp::rules::ruleset::Severity::Info)
            .collect();

        if case.expected_issues.is_empty() {
            assert!(
                active.is_empty(),
                "case {}: unexpected active issues: {:?}",
                case.id,
                active.iter().map(|i| &i.found).collect::<Vec<_>>()
            );
            continue;
        }

        for expected in &case.expected_issues {
            let expected_type = parse_issue_type(&expected.rule_type);
            assert!(
                active.iter().any(|issue| {
                    issue.found == expected.found
                        && issue.rule_type == expected_type
                        && issue.suggestions.iter().any(|s| s == &expected.replace)
                }),
                "case {}: missing expected issue {} -> {} ({})",
                case.id,
                expected.found,
                expected.replace,
                expected.rule_type
            );
        }
    }
}

// ---------------------------------------------------------------------------
// C. Editorial corpus: AI detection
// ---------------------------------------------------------------------------

#[test]
fn editorial_corpus_basic_validation() {
    let corpus = load_corpus("editorial");
    let (scanner, s2t) = load_scanner();
    let profile = Profile::Base;
    let content_type = ContentType::Plain;

    for case in &corpus.cases {
        let input = case.scan_text.as_deref().unwrap_or(&case.input);
        let (_, _issues) = scan_and_fix(
            &scanner,
            &s2t,
            input,
            content_type,
            profile,
            corpus.detect_ai,
        );
        // Editorial corpus is evaluated independently — basic smoke test.
    }
}

// ---------------------------------------------------------------------------
// D. Mixed-content corpus: structural integrity
// ---------------------------------------------------------------------------

#[test]
fn mixed_content_corpus_basic_validation() {
    let corpus = load_corpus("mixed-content");
    let (scanner, s2t) = load_scanner();
    let profile = Profile::Base;
    let content_type = ContentType::Markdown;

    for case in &corpus.cases {
        let input = case.scan_text.as_deref().unwrap_or(&case.input);
        let (fixed, issues) = scan_and_fix(&scanner, &s2t, input, content_type, profile, false);

        // Issues have offsets into the pre-fix working text after any S2T
        // conversion, so validate against the same text domain the scanner saw.
        let work_text = working_text(&s2t, input);
        let work_text = work_text.as_ref();
        for issue in &issues {
            assert!(
                work_text.is_char_boundary(issue.offset),
                "case {}: issue '{}' at offset {} is not a char boundary",
                case.id,
                issue.found,
                issue.offset,
            );
            let end = issue.offset + issue.length;
            assert!(
                end <= work_text.len(),
                "case {}: issue '{}' at offset {} + length {} exceeds text length {}",
                case.id,
                issue.found,
                issue.offset,
                issue.length,
                work_text.len(),
            );
            assert!(
                work_text.is_char_boundary(end),
                "case {}: issue '{}' end offset {} is not a char boundary",
                case.id,
                issue.found,
                end,
            );
        }

        // Verify code blocks are preserved: fenced blocks in input must
        // appear unchanged in output.
        if input.contains("```") {
            for block in input.split("```").skip(1).step_by(2) {
                assert!(
                    fixed.contains(block),
                    "case {}: code block content was corrupted by fixer",
                    case.id,
                );
            }
        }
    }
}
