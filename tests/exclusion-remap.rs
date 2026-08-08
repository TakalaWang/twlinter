// Exclusion remap validation test.
//
// Proves that the fix-path optimization (remapping exclusion zones instead of
// rebuilding them from scratch) produces scan output identical to a full
// exclusion rebuild. This matters because remap_exclusions is O(E + F) while
// rebuild is O(n) with regex compilation cost on every re-scan.

use std::fmt::Write;
use std::time::Instant;

use zhtw_mcp::engine::excluded::ByteRange;
use zhtw_mcp::engine::scan::{build_exclusions_for_content_type, ContentType, Scanner};
use zhtw_mcp::fixer::{apply_fixes_with_context, remap_exclusions, FixMode};
use zhtw_mcp::rules::ruleset::{Profile, RuleType, SpellingRule};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn cross_strait(from: &str, to: &str) -> SpellingRule {
    SpellingRule {
        from: from.into(),
        to: vec![to.into()],
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

/// Build a ~50KB Markdown document with URLs, code blocks, and fixable terms.
///
/// The document interleaves prose paragraphs containing cross-strait terms
/// with code blocks and URLs that must be excluded from scanning. This
/// exercises both the exclusion builder and the remap path.
fn build_test_document(rules: &[(&str, &str)]) -> String {
    let mut doc = String::with_capacity(55_000);

    // Frontmatter (excluded by markdown parser).
    doc.push_str("---\ntitle: 測試文件\ndate: 2026-03-25\n---\n\n");

    // Header.
    doc.push_str("# 排除區域重映射驗證\n\n");

    // Prose paragraph template with a URL and a fixable term per iteration.
    // We cycle through rules to get at least 20 distinct fixable terms.
    let filler_sentences = [
        "在現代作業系統中，記憶體管理是關鍵議題。",
        "程式設計師需要理解各種資料結構的優缺點。",
        "隨著雲端運算的發展，分散式系統越來越重要。",
        "臺灣的資訊產業在全球供應鏈中扮演重要角色。",
        "自由軟體與開放原始碼運動改變了軟體開發的生態。",
    ];

    let urls = [
        "https://example.com/path/to/resource?q=test&lang=zh-TW",
        "https://zh.wikipedia.org/wiki/作業系統",
        "https://docs.rs/tokio/latest/tokio/index.html",
        "https://github.com/user/repo/blob/main/src/lib.rs",
        "https://www.moedict.tw/軟體",
    ];

    let code_blocks = [
        "```rust\nfn main() {\n    println!(\"Hello, world!\");\n}\n```",
        "```python\ndef process(data):\n    return sorted(data)\n```",
        "```c\n#include <stdio.h>\nint main() { return 0; }\n```",
        "```bash\ncurl -s https://api.example.com/data | jq '.items[]'\n```",
        "```json\n{\"key\": \"value\", \"count\": 42}\n```",
    ];

    // Generate enough paragraphs to reach ~50KB.
    // Each paragraph is roughly 500-800 bytes of UTF-8 CJK text.
    let mut para_count = 0;
    while doc.len() < 50_000 {
        let rule_idx = para_count % rules.len();
        let filler_idx = para_count % filler_sentences.len();
        let url_idx = para_count % urls.len();
        let code_idx = para_count % code_blocks.len();

        let (from, _to) = rules[rule_idx];

        // Prose paragraph with the fixable term and a URL.
        writeln!(doc, "## 第 {} 節\n", para_count + 1).unwrap();
        writeln!(
            doc,
            "{}在這個段落中，我們討論{}的相關概念。\
             根據資料來源 {} 所述，\
             {}更多背景資訊請參閱相關文獻。\n",
            filler_sentences[filler_idx],
            from,
            urls[url_idx],
            filler_sentences[(filler_idx + 1) % filler_sentences.len()],
        )
        .unwrap();

        // Inline code reference (should be excluded).
        writeln!(doc, "使用 `{}` 指令可以查看詳細說明。\n", from).unwrap();

        // Code block (should be excluded entirely).
        if para_count % 3 == 0 {
            writeln!(doc, "{}\n", code_blocks[code_idx]).unwrap();
        }

        // Another prose paragraph with the same term to get multiple hits.
        writeln!(
            doc,
            "綜合以上分析，{}在臺灣的用法與中國大陸有顯著差異。\
             相關連結：{}\n",
            from,
            urls[(url_idx + 1) % urls.len()],
        )
        .unwrap();

        // File path (should be excluded).
        if para_count % 4 == 0 {
            writeln!(doc, "設定檔位於 /etc/config/{}.toml 路徑下。\n", from).unwrap();
        }

        para_count += 1;
    }

    doc
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test]
fn remap_exclusions_produces_identical_rescan_output() {
    // Cross-strait terms that have exactly one suggestion and no context_clues,
    // so they fire unconditionally and are eligible for LexicalSafe fixing.
    let term_pairs: Vec<(&str, &str)> = vec![
        ("軟件", "軟體"),
        ("信息", "資訊"),
        ("U盤", "隨身碟"),
        ("主板", "主機板"),
        ("串行", "序列"),
        ("並發", "並行"),
        ("下溢", "下限溢位"),
        ("主線程", "主執行緒"),
        ("並集", "聯集"),
        ("串行端口", "序列埠"),
        ("二分查找", "二分搜尋法"),
        ("交換機", "交換器"),
        ("人機交互", "人機互動"),
        ("代碼段", "程式碼區段"),
        ("位圖", "點陣圖"),
        ("作業調度", "工作排程"),
        ("全角", "全形"),
        ("內聯", "行內"),
        ("冒泡排序", "氣泡排序"),
        ("分佈式", "分散式"),
        ("刻錄機", "燒錄機"),
        ("加載", "載入"),
        ("單片機", "微控制器"),
        ("回調", "回呼"),
        ("固態硬盤", "固態硬碟"),
    ];

    let spelling_rules: Vec<SpellingRule> = term_pairs
        .iter()
        .map(|&(from, to)| cross_strait(from, to))
        .collect();

    let scanner = Scanner::new(spelling_rules, vec![]);
    let cfg = Profile::Base.config();
    let content_type = ContentType::Markdown;

    // Build the test document.
    let document = build_test_document(&term_pairs);
    assert!(
        document.len() >= 50_000,
        "document too small: {} bytes (need >= 50000)",
        document.len()
    );

    // -- Step 1: Initial scan --
    let initial_output = scanner.scan_for_content_type_with_config(&document, content_type, cfg);
    let initial_count = initial_output.issues.len();
    assert!(
        initial_count >= 20,
        "expected at least 20 fixable issues, got {}",
        initial_count
    );

    // Build exclusions for the original text (we need these for the remap path).
    let original_exclusions = build_exclusions_for_content_type(&document, content_type);

    // -- Step 2: Apply fixes --
    let excluded_offsets: Vec<(usize, usize)> = original_exclusions
        .iter()
        .map(|r| (r.start, r.end))
        .collect();

    let fix_result = apply_fixes_with_context(
        &document,
        &initial_output.issues,
        FixMode::LexicalSafe,
        &excluded_offsets,
        None, // no segmenter needed for non-clue rules
    );
    assert!(
        fix_result.applied > 0,
        "expected at least 1 fix applied, got 0"
    );

    let fixed_text = &fix_result.text;

    // -- Step 3a: OLD path -- full exclusion rebuild --
    let t_old = Instant::now();
    let old_output = scanner.scan_for_content_type_with_config(fixed_text, content_type, cfg);
    let old_elapsed = t_old.elapsed();

    // -- Step 3b: NEW path -- remap exclusions, then scan with prebuilt --
    let t_new = Instant::now();
    let remapped = remap_exclusions(&original_exclusions, &fix_result.applied_fixes);
    let new_output =
        scanner.scan_with_prebuilt_excluded_config(fixed_text, &remapped, cfg, content_type);
    let new_elapsed = t_new.elapsed();

    // -- Assertions: identical output --
    assert_eq!(
        old_output.issues.len(),
        new_output.issues.len(),
        "issue count mismatch: old={}, new={}",
        old_output.issues.len(),
        new_output.issues.len()
    );

    for (i, (old_issue, new_issue)) in old_output
        .issues
        .iter()
        .zip(new_output.issues.iter())
        .enumerate()
    {
        assert_eq!(
            old_issue.offset, new_issue.offset,
            "issue #{}: offset mismatch (old={}, new={})",
            i, old_issue.offset, new_issue.offset
        );
        assert_eq!(
            old_issue.length, new_issue.length,
            "issue #{}: length mismatch (old={}, new={})",
            i, old_issue.length, new_issue.length
        );
        assert_eq!(
            old_issue.found, new_issue.found,
            "issue #{}: found mismatch (old='{}', new='{}')",
            i, old_issue.found, new_issue.found
        );
        assert_eq!(
            old_issue.line, new_issue.line,
            "issue #{}: line mismatch (old={}, new={})",
            i, old_issue.line, new_issue.line
        );
        assert_eq!(
            old_issue.suggestions, new_issue.suggestions,
            "issue #{}: suggestions mismatch",
            i
        );
    }

    // -- Optional: print timing --
    eprintln!(
        "exclusion-remap benchmark: old (rebuild) = {:?}, new (remap) = {:?}, \
         fixes applied = {}, residual issues = {}",
        old_elapsed,
        new_elapsed,
        fix_result.applied,
        old_output.issues.len()
    );
}

/// Verify remap correctness with zero fixes applied (identity case).
#[test]
fn remap_exclusions_identity_when_no_fixes() {
    let exclusions = vec![
        ByteRange { start: 10, end: 50 },
        ByteRange {
            start: 100,
            end: 200,
        },
        ByteRange {
            start: 500,
            end: 600,
        },
    ];

    let remapped = remap_exclusions(&exclusions, &[]);
    assert_eq!(
        exclusions, remapped,
        "remap with no fixes should return identical ranges"
    );
}

/// Verify that remap shifts exclusions correctly for a simple single fix.
#[test]
fn remap_exclusions_shifts_after_shorter_replacement() {
    use zhtw_mcp::fixer::AppliedFix;

    // Original text: "AAAA軟件BBBB" where 軟件 is at offset 4, length 6 bytes.
    // Fix: 軟件 (6 bytes) -> 軟體 (6 bytes) -- same length, no shift.
    // But if replacement is shorter: e.g. "XX" (2 bytes), delta = -4.
    let exclusions = vec![ByteRange { start: 20, end: 30 }];

    let fixes = vec![AppliedFix {
        offset: 4,
        old_len: 6,
        replacement: "AB".into(), // 2 bytes, delta = -4
    }];

    let remapped = remap_exclusions(&exclusions, &fixes);
    assert_eq!(remapped.len(), 1);
    assert_eq!(
        remapped[0],
        ByteRange { start: 16, end: 26 },
        "exclusion should shift left by 4 bytes"
    );
}

/// Verify that remap shifts exclusions correctly for a longer replacement.
#[test]
fn remap_exclusions_shifts_after_longer_replacement() {
    use zhtw_mcp::fixer::AppliedFix;

    let exclusions = vec![ByteRange { start: 20, end: 30 }];

    let fixes = vec![AppliedFix {
        offset: 4,
        old_len: 3,                   // 3 bytes original
        replacement: "ABCDEF".into(), // 6 bytes, delta = +3
    }];

    let remapped = remap_exclusions(&exclusions, &fixes);
    assert_eq!(remapped.len(), 1);
    assert_eq!(
        remapped[0],
        ByteRange { start: 23, end: 33 },
        "exclusion should shift right by 3 bytes"
    );
}
