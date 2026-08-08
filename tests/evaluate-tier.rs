// Tier distribution evaluation: measures LLM avoidance across corpora.
//
// The three-tier pipeline automatically resolves/suppresses issues locally
// where possible, sending only genuine gray-zone ambiguity to Tier 3 (LLM).
// No user configuration needed — the system dynamically handles the tradeoff.

use zhtw_mcp::engine::disambig::{disambiguate_batch, DisambigConfig, DisambigStats};
use zhtw_mcp::engine::s2t::S2TConverter;
use zhtw_mcp::engine::scan::{ContentType, Scanner};
use zhtw_mcp::rules::ruleset::Profile;

fn load_scanner() -> (Scanner, S2TConverter) {
    let ruleset = zhtw_mcp::rules::loader::load_embedded_ruleset().unwrap();
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);
    (scanner, S2TConverter::new())
}

struct TierReport {
    corpus: String,
    profile: Profile,
    total_issues: usize,
    not_eligible: usize,
    hard_anchor: usize,
    tier2_resolved: usize,
    suppressed: usize,
    gray_zone: usize,
}

impl TierReport {
    fn llm_avoidance_pct(&self) -> f64 {
        let total_eligible =
            self.hard_anchor + self.tier2_resolved + self.suppressed + self.gray_zone;
        if total_eligible == 0 {
            return 100.0;
        }
        let avoided = self.hard_anchor + self.tier2_resolved + self.suppressed;
        (avoided as f64) / (total_eligible as f64) * 100.0
    }
}

fn evaluate_texts(
    scanner: &Scanner,
    s2t: &S2TConverter,
    corpus_name: &str,
    texts: &[&str],
    content_type: ContentType,
    profile: Profile,
    detect_ai: bool,
) -> TierReport {
    let mut agg_total = 0usize;
    let mut agg = DisambigStats::default();

    for text in texts {
        let converted = if zhtw_mcp::engine::zhtype::detect_chinese_type(text)
            == zhtw_mcp::engine::zhtype::ChineseType::Simplified
        {
            Some(s2t.convert(text))
        } else {
            None
        };
        let work_text = converted.as_deref().unwrap_or(text);

        let mut cfg = profile.config();
        if detect_ai {
            cfg.ai_filler_detection = true;
            cfg.ai_semantic_safety = true;
            cfg.ai_density_detection = true;
            cfg.ai_structural_patterns = true;
        }

        let scan_out = scanner.scan_for_content_type_with_config(work_text, content_type, cfg);
        let mut issues = scan_out.issues;
        agg_total += issues.len();

        let disambig_cfg = DisambigConfig {
            profile,
            ..Default::default()
        };
        let stats = disambiguate_batch(&mut issues, work_text, &disambig_cfg);
        agg.not_eligible += stats.not_eligible;
        agg.hard_anchor += stats.hard_anchor;
        agg.tier2_resolved += stats.tier2_resolved;
        agg.suppressed += stats.suppressed;
        agg.gray_zone += stats.gray_zone;
    }

    TierReport {
        corpus: corpus_name.to_string(),
        profile,
        total_issues: agg_total,
        not_eligible: agg.not_eligible,
        hard_anchor: agg.hard_anchor,
        tier2_resolved: agg.tier2_resolved,
        suppressed: agg.suppressed,
        gray_zone: agg.gray_zone,
    }
}

fn print_report(reports: &[TierReport]) {
    eprintln!();
    eprintln!(
        "{:<20} {:<8} {:>6} {:>8} {:>6} {:>8} {:>6} {:>6} {:>8}",
        "Corpus", "Profile", "Total", "NotElig", "Hard", "Tier2", "Supp", "Gray", "Avoid%"
    );
    eprintln!("{}", "-".repeat(90));
    for r in reports {
        eprintln!(
            "{:<20} {:<8} {:>6} {:>8} {:>6} {:>8} {:>6} {:>6} {:>7.1}%",
            r.corpus,
            r.profile.name(),
            r.total_issues,
            r.not_eligible,
            r.hard_anchor,
            r.tier2_resolved,
            r.suppressed,
            r.gray_zone,
            r.llm_avoidance_pct(),
        );
    }
    eprintln!();
}

// -- Corpus A: deterministic (pure Tier 1, no ambiguity) --------------------

#[test]
fn eval_deterministic_tier_distribution() {
    let (scanner, s2t) = load_scanner();
    let texts = vec![
        "用户可以在信息中查看软件的默认配置。",
        "這是一個測試,請注意!",
        "作業系統的記憶體管理模組已經最佳化。",
    ];
    let report = evaluate_texts(
        &scanner,
        &s2t,
        "deterministic",
        &texts,
        ContentType::Markdown,
        Profile::Base,
        false,
    );
    print_report(&[report]);
}

// -- Corpus B: CN cross-strait (bulk terminology) ---------------------------

#[test]
fn eval_cross_strait_tier_distribution() {
    let (scanner, s2t) = load_scanner();
    let texts = vec![
        "該軟件模塊需要優化數據庫的性能，並把內存中的緩存數據同步到網絡節點。",
        "操作系統提供鏈接檢查器，可分析進程記錄並生成日志文件。",
        "服務器的默認配置需要更新，用戶可以通過網絡接口訪問相關信息。",
        "視頻會議服務使用默認錄像模板，並在文檔頁面顯示錯誤消息。",
        "該程序支持多線程併發處理和異步回調機制。",
    ];
    let report = evaluate_texts(
        &scanner,
        &s2t,
        "cross_strait",
        &texts,
        ContentType::Plain,
        Profile::Base,
        false,
    );
    print_report(&[report]);
}

// -- Corpus C: ambiguous terms (polysemous, context-dependent) --------------

#[test]
fn eval_ambiguous_tier_distribution() {
    let (scanner, s2t) = load_scanner();
    let texts = vec![
        "作業系統的進程排程器管理CPU執行緒分配。",
        "GPU渲染引擎支持即時光線追蹤。",
        "學習的進程需要耐心和毅力。",
        "這個算法支持並行計算和分佈式處理。",
        "用戶端口需要配置防火牆規則。",
    ];
    let report = evaluate_texts(
        &scanner,
        &s2t,
        "ambiguous",
        &texts,
        ContentType::Plain,
        Profile::Base,
        false,
    );
    print_report(&[report]);
}

// -- Corpus D: mixed markdown (structural integrity) ------------------------

#[test]
fn eval_mixed_content_tier_distribution() {
    let (scanner, s2t) = load_scanner();
    let texts = vec![
        "```\nconst x = 1;\n```\n\n該軟件的默認配置需要更新。",
        "使用 `malloc` 函数分配內存空間。",
        "| 名稱 | 說明 |\n|------|------|\n| 記憶體 | 系統資源 |",
        "安裝 Docker 容器並設定 MongoDB 資料庫。",
    ];
    let report = evaluate_texts(
        &scanner,
        &s2t,
        "mixed_content",
        &texts,
        ContentType::Markdown,
        Profile::Base,
        false,
    );
    print_report(&[report]);
}

// -- Aggregated summary across all corpora ----------------------------------

#[test]
fn eval_aggregate_tier_summary() {
    let (scanner, s2t) = load_scanner();

    let all_texts = vec![
        // Deterministic
        "用户可以在信息中查看软件的默认配置。",
        "這是一個測試,請注意!",
        "作業系統的記憶體管理模組已經最佳化。",
        // Cross-strait
        "該軟件模塊需要優化數據庫的性能，並把內存中的緩存數據同步到網絡節點。",
        "操作系統提供鏈接檢查器，可分析進程記錄並生成日志文件。",
        "服務器的默認配置需要更新，用戶可以通過網絡接口訪問相關信息。",
        "視頻會議服務使用默認錄像模板，並在文檔頁面顯示錯誤消息。",
        "該程序支持多線程併發處理和異步回調機制。",
        // Ambiguous
        "作業系統的進程排程器管理CPU執行緒分配。",
        "GPU渲染引擎支持即時光線追蹤。",
        "學習的進程需要耐心和毅力。",
        "這個算法支持並行計算和分佈式處理。",
        "用戶端口需要配置防火牆規則。",
        // Mixed
        "```\nconst x = 1;\n```\n\n該軟件的默認配置需要更新。",
        "使用 `malloc` 函数分配內存空間。",
        "| 名稱 | 說明 |\n|------|------|\n| 記憶體 | 系統資源 |",
        "安裝 Docker 容器並設定 MongoDB 資料庫。",
    ];

    let base = evaluate_texts(
        &scanner,
        &s2t,
        "AGGREGATE",
        &all_texts,
        ContentType::Markdown,
        Profile::Base,
        false,
    );
    let strict = evaluate_texts(
        &scanner,
        &s2t,
        "AGGREGATE",
        &all_texts,
        ContentType::Markdown,
        Profile::Strict,
        false,
    );

    eprintln!();
    eprintln!("=== TIER DISTRIBUTION SUMMARY ===");
    print_report(&[base, strict]);
}

// -- Large CN corpus --------------------------------------------------------

#[test]
fn eval_large_cn_corpus_tier_distribution() {
    let (scanner, s2t) = load_scanner();
    let texts = vec![
        "軟件模組需要優化數據庫的性能,並把內存中的緩存數據同步到網絡節點。",
        "視頻會議服務使用默認錄像範本,並在文檔頁面顯示錯誤消息。",
        "操作系統提供鏈接檢查器，可分析進程記錄。",
        "軟件更新可以優化數據庫緩存。",
        "系統報告可以通過網絡接口訪問,日志文件記錄了進程狀態和異步操作的信息。",
        "該服務器在記錄日誌時發現了內存泄漏問題,需要優化數據庫連接池配置。",
        "操作系統的進程管理器可以監控線程狀態,並通過信號機制處理異步事件。",
        "服務器集群使用默認的負載均衡配置,通過網絡接口分發用戶請求到不同節點。",
    ];
    let report = evaluate_texts(
        &scanner,
        &s2t,
        "cn_to_tw_large",
        &texts,
        ContentType::Plain,
        Profile::Base,
        false,
    );

    eprintln!();
    eprintln!("=== CN-TO-TW LARGE CORPUS (8 production sentences) ===");
    print_report(&[report]);
}
