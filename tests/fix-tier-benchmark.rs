// Benchmark for fix tier precision.
//
// Demonstrates how each fix tier applies progressively more fixes on
// realistic mixed-issue text.  Measures: issues detected, fixes applied,
// fixes skipped, and residual issues per tier.
//
// Run:
//   cargo test --test fix_tier_benchmark -- --nocapture

use std::time::Instant;
use zhtw_mcp::engine::scan::Scanner;
use zhtw_mcp::engine::segment::Segmenter;
use zhtw_mcp::fixer::{apply_fixes_with_context, FixMode};
use zhtw_mcp::rules::ruleset::Ruleset;

fn load_scanner() -> (Scanner, Segmenter) {
    let json_str = include_str!("../assets/ruleset.json");
    let ruleset: Ruleset = serde_json::from_str(json_str).unwrap();
    let segmenter = Segmenter::from_rules(&ruleset.spelling_rules);
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);
    (scanner, segmenter)
}

// Mixed text with orthographic issues (punctuation, spacing) and lexical
// issues (cross-strait terms, confusable terms with context clues).
const MIXED_TEXT: &str = "\
軟體工程師需要優化數據庫的性能,通過調試程序來排查代碼中的問題。\
這個操作系統支持並行計算,能夠充分利用多核處理器的性能優勢。\
請使用內存中的緩存數據進行網絡通訊。\
開發人員利用調試工具來優化軟件的性能。\
我需要編寫一個程序來執行。\
(括號內容)該應用需要響應式設計。";

// Clean zh-TW text -- no issues expected.
const CLEAN_TEXT: &str = "\
軟體工程師需要最佳化資料庫的效能，透過除錯程式來排查程式碼中的問題。\
請使用記憶體中的快取資料進行網路通訊。\
這個作業系統支援平行計算，能夠充分利用多核處理器的效能優勢。";

// Large text: repeat MIXED_TEXT to ~10KB for latency measurement.
fn make_large_text() -> String {
    MIXED_TEXT.repeat(20)
}

#[derive(Debug)]
struct TierResult {
    tier: &'static str,
    issues_detected: usize,
    fixes_applied: usize,
    fixes_skipped: usize,
    residual_issues: usize,
    elapsed_us: u128,
}

fn run_tier(
    scanner: &Scanner,
    segmenter: &Segmenter,
    text: &str,
    mode: FixMode,
    tier_name: &'static str,
) -> TierResult {
    let start = Instant::now();

    let scan_out = scanner.scan(text);
    let issues_detected = scan_out.issues.len();

    let excluded_pairs: Vec<(usize, usize)> = Vec::new();
    let fix_result = apply_fixes_with_context(
        text,
        &scan_out.issues,
        mode,
        &excluded_pairs,
        Some(segmenter),
    );

    // Re-scan fixed text to count residual issues.
    let rescan = scanner.scan(&fix_result.text);
    let elapsed = start.elapsed();

    TierResult {
        tier: tier_name,
        issues_detected,
        fixes_applied: fix_result.applied,
        fixes_skipped: fix_result.skipped,
        residual_issues: rescan.issues.len(),
        elapsed_us: elapsed.as_micros(),
    }
}

#[test]
fn fix_tier_precision_gradient() {
    let (scanner, segmenter) = load_scanner();

    println!();
    println!("=== Fix Tier Precision Benchmark ===");
    println!();

    // Run all tiers on the mixed text.
    let tiers = [
        (FixMode::None, "none"),
        (FixMode::Orthographic, "orthographic"),
        (FixMode::LexicalSafe, "lexical_safe"),
        (FixMode::LexicalContextual, "lexical_contextual"),
    ];

    println!("--- Mixed text ({} bytes) ---", MIXED_TEXT.len());
    println!(
        "{:<22} {:>8} {:>8} {:>8} {:>10} {:>10}",
        "tier", "detected", "applied", "skipped", "residual", "time_us"
    );
    println!("{}", "-".repeat(72));

    let mut results = Vec::new();
    for (mode, name) in &tiers {
        let r = run_tier(&scanner, &segmenter, MIXED_TEXT, *mode, name);
        println!(
            "{:<22} {:>8} {:>8} {:>8} {:>10} {:>10}",
            r.tier,
            r.issues_detected,
            r.fixes_applied,
            r.fixes_skipped,
            r.residual_issues,
            r.elapsed_us
        );
        results.push(r);
    }

    // Verify the strict superset property: each tier applies strictly more
    // than the previous (catches regressions where tiers collapse).
    for i in 1..results.len() {
        assert!(
            results[i].fixes_applied > results[i - 1].fixes_applied,
            "{} did not apply strictly more fixes ({}) than {} ({})",
            results[i].tier,
            results[i].fixes_applied,
            results[i - 1].tier,
            results[i - 1].fixes_applied,
        );
    }

    // Verify the residual monotonicity: each tier leaves strictly fewer
    // residual issues than the previous.
    for i in 1..results.len() {
        assert!(
            results[i].residual_issues < results[i - 1].residual_issues,
            "{} did not leave strictly fewer residual issues ({}) than {} ({})",
            results[i].tier,
            results[i].residual_issues,
            results[i - 1].tier,
            results[i - 1].residual_issues,
        );
    }

    println!();
    println!("--- Clean text ({} bytes) ---", CLEAN_TEXT.len());
    let clean_result = run_tier(
        &scanner,
        &segmenter,
        CLEAN_TEXT,
        FixMode::LexicalContextual,
        "lexical_contextual",
    );
    println!(
        "  detected={}, applied={}, residual={}, time={}us",
        clean_result.issues_detected,
        clean_result.fixes_applied,
        clean_result.residual_issues,
        clean_result.elapsed_us
    );
    // Clean text should have 0 issues.
    assert_eq!(
        clean_result.issues_detected, 0,
        "clean zh-TW text should have 0 issues, got {}",
        clean_result.issues_detected
    );

    println!();
    println!(
        "--- Large text (~10KB, {} bytes) ---",
        make_large_text().len()
    );
    let large = make_large_text();
    println!(
        "{:<22} {:>8} {:>8} {:>8} {:>10} {:>10}",
        "tier", "detected", "applied", "skipped", "residual", "time_us"
    );
    println!("{}", "-".repeat(72));
    for (mode, name) in &tiers[1..] {
        let r = run_tier(&scanner, &segmenter, &large, *mode, name);
        println!(
            "{:<22} {:>8} {:>8} {:>8} {:>10} {:>10}",
            r.tier,
            r.issues_detected,
            r.fixes_applied,
            r.fixes_skipped,
            r.residual_issues,
            r.elapsed_us
        );
    }

    println!();
    println!("Key: detected=issues before fix, applied=fixes made, skipped=fixes refused, residual=issues after fix");
}
