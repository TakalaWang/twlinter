// Startup / cold-first-request profiler for zhtw-core.
//
// Not a criterion benchmark: cold-start is a once-per-process event. Criterion
// runs many iterations, so its lazy statics (grammar AC, zhtype char tables,
// clue index) are warm after iteration 1 -- it structurally cannot see the
// cold cost. This binary measures each phase exactly once, in order:
//
//   ruleset_load  postcard deserialize of the embedded artifact
//   scanner_build Aho-Corasick + segmenter construction (what Server::new does)
//   [server ready = load + build]
//   first_scan    first tools/call scan -- pays lazy init of grammar AC etc.
//   warm_scan     steady-state scan (min of a few iters)
//   [cold overhead = first_scan - warm_scan]
//
// Run: cargo run --release --bench startup
// The 50ms cold-start budget is checked against `server ready + first_scan`;
// above it, prewarming the lazy first-hit structures during init pays off.

use std::time::Instant;

use zhtw_core::engine::scan::Scanner;
use zhtw_core::rules::loader::load_embedded_ruleset;
use zhtw_core::rules::ruleset::Profile;

// Representative first-request text: mixed CJK prose with flaggable Mainland
// terms plus ASCII, sized like a typical paragraph a client sends first.
const FIRST_REQUEST: &str = "\
台灣的軟件工程師使用人工智能技術開發應用程序，\
他們的網絡質量很高，信息安全也很重要。\
The server handles HTTP requests via async runtime.\n\
數據庫中存儲了用戶的個人信息和視頻文件。";

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    let t_load = Instant::now();
    let ruleset = load_embedded_ruleset().expect("load embedded ruleset");
    let load = t_load.elapsed();

    let t_build = Instant::now();
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);
    let build = t_build.elapsed();

    // First cold scan: identical code path to a tools/call, so it triggers
    // every first-hit lazy structure (grammar AC, SIMPLIFIED/TRADITIONAL char
    // sets, clue index) exactly as the first real request would.
    let t_first = Instant::now();
    let first = scanner.scan_profiled(FIRST_REQUEST, Profile::Base);
    let first_scan = t_first.elapsed();
    if first.issues.is_empty() {
        // Not a failure -- the scanner works, the sample text just stopped
        // hitting any rule. Warn so the cold measurement isn't trusted blindly.
        eprintln!("warning: FIRST_REQUEST produced no issues; sample text may be stale");
    }

    // Warm steady state: min of a handful of iterations, statics now hot.
    let mut warm = std::time::Duration::MAX;
    for _ in 0..64 {
        let t = Instant::now();
        let out = scanner.scan_profiled(FIRST_REQUEST, Profile::Base);
        warm = warm.min(t.elapsed());
        std::hint::black_box(&out);
    }

    let ready = load + build;
    let cold_overhead = first_scan.saturating_sub(warm);
    let cold_first_request = ready + first_scan;

    println!(
        "startup profile ({} bytes first request)",
        FIRST_REQUEST.len()
    );
    println!("  ruleset_load        {:>8.3} ms", ms(load));
    println!("  scanner_build       {:>8.3} ms", ms(build));
    println!("  = server ready      {:>8.3} ms", ms(ready));
    println!("  first_scan (cold)   {:>8.3} ms", ms(first_scan));
    println!("  warm_scan           {:>8.3} ms", ms(warm));
    println!(
        "  = cold init overhead{:>8.3} ms  (first_scan - warm)",
        ms(cold_overhead)
    );
    println!(
        "  = cold 1st request  {:>8.3} ms  (ready + first_scan)",
        ms(cold_first_request)
    );

    let gate = 50.0;
    if ms(cold_first_request) > gate {
        println!("\nover {gate:.0}ms gate -> prewarming worth it");
    } else {
        println!("\nunder {gate:.0}ms gate -> prewarming not justified");
    }
}
