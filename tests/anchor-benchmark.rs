// Benchmark for calibration-based anchor verification.
//
// Measures: anchor coverage, match/unmatch rate, latency overhead,
// and multi-run convergence.
//
// Requires network access (Google Translate API) so marked #[ignore] by default.
// Run explicitly:
//   cargo test --test anchor_benchmark -- --ignored --nocapture
//
// Set CORPUS_DIR=path/to/dir to lint real-world .md/.txt files instead of
// the built-in synthetic corpus.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

fn binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("zhtw-mcp");
    path
}

fn run_lint_json(input: &str, extra_args: &[&str]) -> (String, std::time::Duration) {
    let bin = binary_path();
    let start = Instant::now();
    let output = Command::new(&bin)
        .args(["lint", "--"])
        .args(["--format", "json"])
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            child.wait_with_output()
        })
        .unwrap();
    let elapsed = start.elapsed();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    (stdout, elapsed)
}

/// Synthetic corpus: cross-strait terms in realistic sentence contexts.
fn synthetic_corpus() -> &'static str {
    concat!(
        // IT terms
        "這個軟件的質量很好，內存佔用也很低。\n",
        "我們正在優化視頻播放器的渲染引擎。\n",
        "請檢查服務器的網絡帶寬是否足夠。\n",
        "程序員在調試這段代碼的時候發現了問題。\n",
        "硬盤空間不足，需要清理緩存文件。\n",
        "該操作系統的內核支持多線程並發處理。\n",
        "移動端應用需要適配不同的分辨率。\n",
        "在雲計算平台上部署容器化的微服務架構。\n",
        // General terms
        "他的表達方式非常簡練。\n",
        "這個方案的信息量很大。\n",
        // Mixed: some terms should fire, some should not
        "使用正確的軟體開發方法論。\n", // 軟體 is zh-TW, should NOT fire
        "正確的繁體中文排版很重要。\n", // clean text
    )
}

#[derive(Debug, Default)]
struct BenchmarkMetrics {
    total_issues: usize,
    issues_with_anchor: usize,
    matched: usize,
    unmatched: usize,
    no_signal: usize,
    baseline_duration: std::time::Duration,
    verify_duration_1: std::time::Duration,
    verify_duration_2: std::time::Duration,
    convergence_identical: bool,
}

fn parse_issues(json_str: &str) -> serde_json::Value {
    serde_json::from_str(json_str).unwrap_or_else(|e| {
        panic!("Failed to parse JSON output: {e}\nRaw output:\n{json_str}");
    })
}

fn count_anchor_stats(issues: &[serde_json::Value]) -> (usize, usize, usize, usize) {
    let mut with_anchor = 0usize;
    let mut matched = 0usize;
    let mut unmatched = 0usize;
    let mut no_signal = 0usize;

    for issue in issues {
        match issue.get("anchor_match") {
            Some(serde_json::Value::Bool(true)) => {
                with_anchor += 1;
                matched += 1;
            }
            Some(serde_json::Value::Bool(false)) => {
                with_anchor += 1;
                unmatched += 1;
            }
            _ => {
                no_signal += 1;
            }
        }
    }
    (with_anchor, matched, unmatched, no_signal)
}

fn collect_metrics(corpus: &str) -> BenchmarkMetrics {
    let mut m = BenchmarkMetrics::default();

    // Run 1: baseline without --verify
    let (baseline_json, baseline_dur) = run_lint_json(corpus, &[]);
    m.baseline_duration = baseline_dur;
    let baseline = parse_issues(&baseline_json);
    m.total_issues = baseline["total"].as_u64().unwrap_or(0) as usize;

    // Run 2: first --verify (single API call, no cache)
    let (verify1_json, verify1_dur) = run_lint_json(corpus, &["--verify"]);
    m.verify_duration_1 = verify1_dur;

    // Run 3: second --verify (for convergence check)
    let (verify2_json, verify2_dur) = run_lint_json(corpus, &["--verify"]);
    m.verify_duration_2 = verify2_dur;

    let v1 = parse_issues(&verify1_json);
    if let Some(issues) = v1["issues"].as_array() {
        let (with_anchor, matched, unmatched, no_signal) = count_anchor_stats(issues);
        m.issues_with_anchor = with_anchor;
        m.matched = matched;
        m.unmatched = unmatched;
        m.no_signal = no_signal;
    }

    // Convergence: both verify runs should produce identical anchor_match values
    let v2 = parse_issues(&verify2_json);
    m.convergence_identical = v1["issues"] == v2["issues"];

    m
}

fn pct(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64 * 100.0
    }
}

fn print_report(m: &BenchmarkMetrics) {
    println!("\n========================================");
    println!("  Calibration Benchmark Report");
    println!("========================================\n");

    println!("1. Issue Summary");
    println!("   Total issues detected:       {}", m.total_issues);
    println!("   Issues with anchor signal:   {}", m.issues_with_anchor);
    println!("   No signal (no english/API):  {}", m.no_signal);
    println!(
        "   Anchor coverage:             {:.1}%",
        pct(m.issues_with_anchor, m.total_issues)
    );
    println!();

    println!("2. Anchor Match Results");
    println!(
        "   Matched (true):              {} ({:.1}%)",
        m.matched,
        pct(m.matched, m.issues_with_anchor)
    );
    println!(
        "   Unmatched (false):           {} ({:.1}%)",
        m.unmatched,
        pct(m.unmatched, m.issues_with_anchor)
    );
    println!();

    println!("3. Latency Analysis");
    println!(
        "   Baseline (no verify):        {:>8.1}ms",
        m.baseline_duration.as_secs_f64() * 1000.0
    );
    println!(
        "   Verify run 1:                {:>8.1}ms",
        m.verify_duration_1.as_secs_f64() * 1000.0
    );
    println!(
        "   Verify run 2:                {:>8.1}ms",
        m.verify_duration_2.as_secs_f64() * 1000.0
    );
    if m.baseline_duration.as_nanos() > 0 {
        let overhead =
            (m.verify_duration_1.as_secs_f64() / m.baseline_duration.as_secs_f64() - 1.0) * 100.0;
        println!("   Overhead vs baseline:        {:>8.1}%", overhead);
    }
    println!();

    println!("4. Convergence (Multi-Run Stability)");
    println!(
        "   Run 1 vs run 2 identical:    {}",
        if m.convergence_identical {
            "YES (stable)"
        } else {
            "NO (non-deterministic)"
        }
    );
    println!();

    println!("========================================\n");
}

#[test]
#[ignore]
fn anchor_benchmark_synthetic() {
    let corpus = synthetic_corpus();
    let m = collect_metrics(corpus);
    print_report(&m);

    assert!(
        m.total_issues >= 5,
        "synthetic corpus should produce at least 5 issues, got {}",
        m.total_issues
    );
}

#[test]
#[ignore]
fn anchor_benchmark_corpus_dir() {
    let corpus_dir = match std::env::var("CORPUS_DIR") {
        Ok(d) => d,
        Err(_) => {
            eprintln!("CORPUS_DIR not set, skipping corpus_dir benchmark");
            return;
        }
    };

    let bin = binary_path();

    // Run 1: baseline
    let start = Instant::now();
    let baseline_output = Command::new(&bin)
        .args(["lint", &corpus_dir, "--format", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let baseline_dur = start.elapsed();

    // Run 2: verify
    let start = Instant::now();
    let verify_output = Command::new(&bin)
        .args(["lint", &corpus_dir, "--format", "json", "--verify"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let verify_dur = start.elapsed();

    let baseline_json = String::from_utf8_lossy(&baseline_output.stdout);
    let verify_json = String::from_utf8_lossy(&verify_output.stdout);

    // zhtw lint exits 0 (clean) or 1 (issues found).  Both are expected for
    // benchmark corpora.  Any other exit code indicates a crash or usage error.
    let baseline_code = baseline_output.status.code().unwrap_or(-1);
    assert!(
        baseline_code == 0 || baseline_code == 1,
        "baseline CLI unexpected exit code {baseline_code}: {}",
        String::from_utf8_lossy(&baseline_output.stderr)
    );
    let verify_code = verify_output.status.code().unwrap_or(-1);
    assert!(
        verify_code == 0 || verify_code == 1,
        "verify CLI unexpected exit code {verify_code}: {}",
        String::from_utf8_lossy(&verify_output.stderr)
    );

    let baseline: serde_json::Value = serde_json::from_str(&baseline_json)
        .unwrap_or_else(|e| panic!("Failed to parse baseline JSON: {e}\nRaw: {baseline_json}"));
    let verified: serde_json::Value = serde_json::from_str(&verify_json)
        .unwrap_or_else(|e| panic!("Failed to parse verify JSON: {e}\nRaw: {verify_json}"));

    // Aggregate across files
    let mut total_issues = 0usize;
    let mut issues_with_anchor = 0usize;
    let mut matched = 0usize;
    let mut unmatched = 0usize;
    let mut no_signal = 0usize;

    let files = if verified.is_array() {
        verified.as_array().unwrap().clone()
    } else {
        vec![verified.clone()]
    };

    for file_result in &files {
        if let Some(issues) = file_result["issues"].as_array() {
            total_issues += issues.len();
            let (wa, ma, um, ns) = count_anchor_stats(issues);
            issues_with_anchor += wa;
            matched += ma;
            unmatched += um;
            no_signal += ns;
        }
    }

    let baseline_files = if baseline.is_array() {
        baseline.as_array().unwrap().clone()
    } else {
        vec![baseline.clone()]
    };
    let baseline_total: usize = baseline_files
        .iter()
        .filter_map(|f| f["total"].as_u64())
        .sum::<u64>() as usize;

    println!("\n========================================");
    println!("  Calibration Benchmark: Corpus Directory");
    println!("  {}", corpus_dir);
    println!("========================================\n");

    println!("Files scanned:              {}", files.len());
    println!("Baseline total issues:      {}", baseline_total);
    println!("Verified total issues:      {}", total_issues);
    println!("Issues with anchor signal:  {}", issues_with_anchor);
    println!(
        "Anchor coverage:            {:.1}%",
        pct(issues_with_anchor, total_issues)
    );
    println!();
    println!(
        "Matched (true):             {} ({:.1}%)",
        matched,
        pct(matched, issues_with_anchor)
    );
    println!(
        "Unmatched (false):          {} ({:.1}%)",
        unmatched,
        pct(unmatched, issues_with_anchor)
    );
    println!("No signal:                  {}", no_signal);
    println!();

    println!(
        "Baseline latency:           {:>8.1}ms",
        baseline_dur.as_secs_f64() * 1000.0
    );
    println!(
        "Verify latency:             {:>8.1}ms",
        verify_dur.as_secs_f64() * 1000.0
    );
    if baseline_dur.as_nanos() > 0 {
        let overhead = (verify_dur.as_secs_f64() / baseline_dur.as_secs_f64() - 1.0) * 100.0;
        println!("Overhead vs baseline:       {:>8.1}%", overhead);
    }

    println!("\n========================================\n");
}
