use serde::Deserialize;
use zhtw_mcp::engine::s2t::S2TConverter;
use zhtw_mcp::engine::scan::{ContentType, Scanner};
use zhtw_mcp::engine::segment::Segmenter;
use zhtw_mcp::fixer::{apply_fixes_with_context, FixMode};
use zhtw_mcp::rules::ruleset::{Issue, IssueType, Profile, ProfileConfig, Ruleset};

#[derive(Debug, Deserialize)]
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
struct CorpusCase {
    id: String,
    repeat: usize,
    input: String,
    #[serde(default)]
    scan_text: Option<String>,
    expected_fixed: String,
    expected_issues: Vec<ExpectedIssue>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedIssue {
    found: String,
    replace: String,
    rule_type: String,
    #[serde(default = "default_occurrence")]
    occurrence: usize,
}

fn default_occurrence() -> usize {
    1
}

#[derive(Debug, Clone)]
struct ResolvedExpectedIssue {
    offset: usize,
    found: String,
    replace: String,
    rule_type: IssueType,
}

#[derive(Debug, Default, Clone)]
struct ScoreCounts {
    tp: usize,
    fp: usize,
    fn_: usize,
}

#[derive(Debug, Default, Clone)]
struct FixCounts {
    exact_docs: usize,
    total_docs: usize,
}

#[derive(Debug, Default)]
struct NativeCounts {
    flagged_docs: usize,
    total_docs: usize,
    total_fp_issues: usize,
}

fn load_scanner() -> (Scanner, Segmenter) {
    let json_str = include_str!("../assets/ruleset.json");
    let ruleset: Ruleset = serde_json::from_str(json_str).unwrap();
    let segmenter = Segmenter::from_rules(&ruleset.spelling_rules);
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);
    (scanner, segmenter)
}

fn build_config(spec: &CorpusSpec) -> ProfileConfig {
    let profile = Profile::from_str_strict(&spec.profile)
        .unwrap_or_else(|| panic!("unknown profile: {}", spec.profile));
    let mut cfg = profile.config();
    if spec.detect_ai {
        cfg.ai_filler_detection = true;
        cfg.ai_semantic_safety = true;
        cfg.ai_density_detection = true;
        cfg.ai_structural_patterns = true;
    }
    cfg
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

fn nth_offset(text: &str, needle: &str, occurrence: usize) -> Option<usize> {
    assert!(occurrence > 0, "occurrence must be >= 1 (1-based)");
    text.match_indices(needle)
        .nth(occurrence - 1)
        .map(|(idx, _)| idx)
}

fn resolve_expected_issues(text: &str, expected: &[ExpectedIssue]) -> Vec<ResolvedExpectedIssue> {
    expected
        .iter()
        .map(|issue| {
            let offset = nth_offset(text, &issue.found, issue.occurrence).unwrap_or_else(|| {
                panic!(
                    "could not resolve occurrence {} of {:?} in text {:?}",
                    issue.occurrence, issue.found, text
                )
            });
            ResolvedExpectedIssue {
                offset,
                found: issue.found.clone(),
                replace: issue.replace.clone(),
                rule_type: parse_issue_type(&issue.rule_type),
            }
        })
        .collect()
}

fn matches_expected(actual: &Issue, expected: &ResolvedExpectedIssue) -> bool {
    actual.offset == expected.offset
        && actual.found == expected.found
        && actual.rule_type == expected.rule_type
        && (actual.suggestions.iter().any(|s| s == &expected.replace)
            || (expected.replace.is_empty() && actual.suggestions.is_empty()))
}

fn score_document(actual: &[Issue], expected: &[ResolvedExpectedIssue]) -> ScoreCounts {
    let mut used = vec![false; actual.len()];
    let mut counts = ScoreCounts::default();

    for exp in expected {
        if let Some(idx) = actual
            .iter()
            .enumerate()
            .find_map(|(idx, issue)| (!used[idx] && matches_expected(issue, exp)).then_some(idx))
        {
            used[idx] = true;
            counts.tp += 1;
        } else {
            counts.fn_ += 1;
        }
    }

    counts.fp = used.iter().filter(|matched| !**matched).count();
    counts
}

fn pct(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64 * 100.0
    }
}

fn precision(counts: &ScoreCounts) -> f64 {
    let denom = counts.tp + counts.fp;
    if denom == 0 {
        100.0
    } else {
        pct(counts.tp, denom)
    }
}

fn recall(counts: &ScoreCounts) -> f64 {
    let denom = counts.tp + counts.fn_;
    if denom == 0 {
        100.0
    } else {
        pct(counts.tp, denom)
    }
}

fn fix_success_rate(counts: &FixCounts) -> f64 {
    pct(counts.exact_docs, counts.total_docs)
}

fn native_fp_rate(counts: &NativeCounts) -> f64 {
    pct(counts.flagged_docs, counts.total_docs)
}

fn evaluate_positive_corpus(
    spec: &CorpusSpec,
    scanner: &Scanner,
    segmenter: &Segmenter,
    converter: &S2TConverter,
) -> (ScoreCounts, FixCounts, usize) {
    let cfg = build_config(spec);
    let mut score = ScoreCounts::default();
    let mut fix = FixCounts::default();
    let mut total_bytes = 0usize;

    for case in &spec.cases {
        let scan_text = if spec.mode == "s2t" {
            let converted = converter.convert(&case.input);
            let expected_scan_text = case
                .scan_text
                .as_ref()
                .unwrap_or_else(|| panic!("{}:{} missing scan_text", spec.id, case.id));
            assert_eq!(
                converted, *expected_scan_text,
                "{}:{} built-in S2T output drifted",
                spec.id, case.id
            );
            expected_scan_text.as_str()
        } else {
            case.input.as_str()
        };

        let expected = resolve_expected_issues(scan_text, &case.expected_issues);
        let issues = scanner
            .scan_for_content_type_with_config(scan_text, ContentType::Plain, cfg)
            .issues;
        let doc_score = score_document(&issues, &expected);
        let fixed = apply_fixes_with_context(
            scan_text,
            &issues,
            FixMode::LexicalSafe,
            &[],
            Some(segmenter),
        );

        total_bytes += scan_text.len() * case.repeat;
        score.tp += doc_score.tp * case.repeat;
        score.fp += doc_score.fp * case.repeat;
        score.fn_ += doc_score.fn_ * case.repeat;
        fix.total_docs += case.repeat;
        if fixed.text == case.expected_fixed {
            fix.exact_docs += case.repeat;
        }
    }

    (score, fix, total_bytes)
}

fn evaluate_native_corpus(
    spec: &CorpusSpec,
    scanner: &Scanner,
    segmenter: &Segmenter,
) -> (NativeCounts, FixCounts, usize) {
    let cfg = build_config(spec);
    let mut native = NativeCounts::default();
    let mut fix = FixCounts::default();
    let mut total_bytes = 0usize;

    for case in &spec.cases {
        let issues = scanner
            .scan_for_content_type_with_config(&case.input, ContentType::Plain, cfg)
            .issues;
        let fixed = apply_fixes_with_context(
            &case.input,
            &issues,
            FixMode::LexicalSafe,
            &[],
            Some(segmenter),
        );

        total_bytes += case.input.len() * case.repeat;
        native.total_docs += case.repeat;
        native.total_fp_issues += issues.len() * case.repeat;
        if !issues.is_empty() {
            native.flagged_docs += case.repeat;
        }
        fix.total_docs += case.repeat;
        if fixed.text == case.expected_fixed {
            fix.exact_docs += case.repeat;
        }
    }

    (native, fix, total_bytes)
}

fn print_positive_report(spec: &CorpusSpec, score: &ScoreCounts, fix: &FixCounts, bytes: usize) {
    println!(
        "{:<24} bytes={:>6}  precision={:>5.1}%  recall={:>5.1}%  safe_fix={:>5.1}%  tp={} fp={} fn={}  {}",
        spec.id,
        bytes,
        precision(score),
        recall(score),
        fix_success_rate(fix),
        score.tp,
        score.fp,
        score.fn_,
        spec.label,
    );
}

fn print_native_report(spec: &CorpusSpec, native: &NativeCounts, fix: &FixCounts, bytes: usize) {
    println!(
        "{:<24} bytes={:>6}  false_positive_rate={:>5.1}%  flagged_docs={}/{}  fp_issues={}  safe_fix={:>5.1}%  {}",
        spec.id,
        bytes,
        native_fp_rate(native),
        native.flagged_docs,
        native.total_docs,
        native.total_fp_issues,
        fix_success_rate(fix),
        spec.label,
    );
}

fn load_corpus(path: &str) -> CorpusSpec {
    serde_json::from_str(path).unwrap()
}

#[test]
fn corpus_evaluation_suite() {
    let (scanner, segmenter) = load_scanner();
    let converter = S2TConverter::new();

    let ai = load_corpus(include_str!("corpus/ai-generated.json"));
    let native = load_corpus(include_str!("corpus/native-zh-tw.json"));
    let cn = load_corpus(include_str!("corpus/cn-to-tw-conversion.json"));

    let (ai_score, ai_fix, ai_bytes) =
        evaluate_positive_corpus(&ai, &scanner, &segmenter, &converter);
    let (native_counts, native_fix, native_bytes) =
        evaluate_native_corpus(&native, &scanner, &segmenter);
    let (cn_score, cn_fix, cn_bytes) =
        evaluate_positive_corpus(&cn, &scanner, &segmenter, &converter);

    assert!(
        ai_bytes >= ai.min_bytes,
        "{} corpus too small: {} < {} bytes",
        ai.id,
        ai_bytes,
        ai.min_bytes
    );
    assert!(
        native_bytes >= native.min_bytes,
        "{} corpus too small: {} < {} bytes",
        native.id,
        native_bytes,
        native.min_bytes
    );
    assert!(
        cn_bytes >= cn.min_bytes,
        "{} corpus too small: {} < {} bytes",
        cn.id,
        cn_bytes,
        cn.min_bytes
    );

    let aggregate = ScoreCounts {
        tp: ai_score.tp + cn_score.tp,
        fp: ai_score.fp + cn_score.fp,
        fn_: ai_score.fn_ + cn_score.fn_,
    };

    println!();
    println!("=== Corpus Evaluation Suite (36.0) ===");
    println!();
    println!("{:<24} {:<}", "corpus", "metrics");
    println!("{}", "-".repeat(112));
    print_positive_report(&ai, &ai_score, &ai_fix, ai_bytes);
    print_native_report(&native, &native_counts, &native_fix, native_bytes);
    print_positive_report(&cn, &cn_score, &cn_fix, cn_bytes);
    println!("{}", "-".repeat(112));
    println!(
        "{:<24} precision={:>5.1}%  recall={:>5.1}%",
        "aggregate_dirty",
        precision(&aggregate),
        recall(&aggregate),
    );
    println!();

    assert!(
        precision(&aggregate) >= 90.0,
        "aggregate precision gate failed: {:.1}%",
        precision(&aggregate)
    );
    assert!(
        native_fp_rate(&native_counts) <= 5.0,
        "native zh-TW false-positive gate failed: {:.1}%",
        native_fp_rate(&native_counts)
    );
    assert!(
        fix_success_rate(&ai_fix) >= 85.0,
        "AI-generated safe-fix gate failed: {:.1}%",
        fix_success_rate(&ai_fix)
    );
}
