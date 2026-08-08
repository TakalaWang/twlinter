// Fixture-driven integration suite for the EN→ZH calque detectors
// (ZY1a / ZY1b / ZY2a / ZY2b / ZY3a / ZY3b / ZY4a / ZY5).
//
// Each fixture in `tests/fixtures/calque/` carries a stable ID in its
// filename: `calque_<rfN>_<detector>_<bad|good|solo>_<NNN>.txt`.  Bad
// fixtures must fire the detector named in the filename (or its
// boundary-aware sibling for ZY2 / ZY3 families, where post-scan dedup
// promotes boundary-aware issues over their substring-only counterparts
// on the same span); good/solo fixtures must NOT fire that detector.

use std::fs;
use std::path::{Path, PathBuf};

use zhtw_mcp::engine::scan::{ContentType, Scanner};
use zhtw_mcp::rules::loader::load_embedded_ruleset;
use zhtw_mcp::rules::ruleset::{Issue, IssueType, Profile};

const FIXTURE_PREFIX: &str = "calque_";

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("calque")
}

fn scan_fixture(text: &str) -> Vec<Issue> {
    let ruleset = load_embedded_ruleset().expect("embedded ruleset loads");
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);
    let out = scanner.scan_for_content_type(text, ContentType::Plain, Profile::Base);
    out.issues
        .into_iter()
        .filter(|i| matches!(i.rule_type, IssueType::Translationese))
        .filter(|i| {
            i.context.as_ref().is_some_and(|c| {
                c.contains("ZY1a")
                    || c.contains("ZY1b")
                    || c.contains("ZY2a")
                    || c.contains("ZY2b")
                    || c.contains("ZY3a")
                    || c.contains("ZY3b")
                    || c.contains("ZY4a")
                    || c.contains("ZY5")
            })
        })
        .collect()
}

/// Extract the detector code from a fixture filename.
///
/// Filename schema: `calque_<category>_<detector>_<bad|good|solo>_<NNN>.txt`,
/// where `<category>` is one of `superlative`, `connective`,
/// `nominalization`, `premodifier`, `falsefriend`, and `<detector>` is
/// one of the registered tokens below.  Parsing is strict — unknown
/// tokens (e.g. a future `zy1c`) return `None` so the test fails loudly
/// instead of silently misclassifying as a known code.
fn detector_in_id(filename: &str) -> Option<&'static str> {
    let stem = filename.strip_suffix(".txt").unwrap_or(filename);
    // Token positions in the schema: 0=calque, 1=category, 2=detector,
    // 3=bad/good/solo, 4=NNN.
    let token = stem.split('_').nth(2)?;
    match token {
        "zy1" => Some("ZY1a"),
        "zy1b" => Some("ZY1b"),
        "zy2" => Some("ZY2a"),
        "zy2b" => Some("ZY2b"),
        "zy3" => Some("ZY3a"),
        "zy3b" => Some("ZY3b"),
        "zy4" => Some("ZY4a"),
        "zy5" => Some("ZY5"),
        _ => None,
    }
}

fn read_fixtures(prefix: &str) -> Vec<(String, String)> {
    let dir = fixtures_dir();
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).expect("fixtures dir exists") {
        let entry = entry.unwrap();
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) if n.starts_with(prefix) && n.ends_with(".txt") => n.to_string(),
            _ => continue,
        };
        let body = fs::read_to_string(&path).expect("readable fixture");
        out.push((name, body));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn calque_bad_fixtures_fire_their_detector() {
    let fixtures = read_fixtures(FIXTURE_PREFIX);
    assert!(
        fixtures.len() >= 24,
        "expected ≥24 fixtures (substring-only + boundary-aware + long-pre-modifier set), found {}",
        fixtures.len()
    );

    let bads: Vec<_> = fixtures
        .iter()
        .filter(|(n, _)| n.contains("_bad_"))
        .collect();
    assert!(!bads.is_empty(), "no bad fixtures");

    for (name, body) in bads {
        let detector = detector_in_id(name).unwrap_or_else(|| {
            panic!("unrecognized detector ID in {name}");
        });
        let issues = scan_fixture(body);
        // Detector-family equivalence: scan_with_config's
        // `dedup_translationese_phase_duplicates` suppresses a substring-
        // only issue (ZY2a / ZY3a) when its boundary-aware sibling (ZY2b /
        // ZY3b) covers the same span.  Substring-only fixture IDs may
        // therefore see their boundary-aware sibling fire instead — both
        // belong to the same family and either satisfies the gate.
        let acceptable: &[&str] = match detector {
            "ZY2a" => &["ZY2a", "ZY2b"],
            "ZY3a" => &["ZY3a", "ZY3b"],
            "ZY1a" => &["ZY1a"],
            "ZY1b" => &["ZY1b"],
            "ZY2b" => &["ZY2b"],
            "ZY3b" => &["ZY3b"],
            "ZY4a" => &["ZY4a"],
            "ZY5" => &["ZY5"],
            other => panic!("unhandled detector code in test: {other}"),
        };
        let fires = issues.iter().any(|i| {
            i.context
                .as_ref()
                .is_some_and(|c| acceptable.iter().any(|code| c.contains(code)))
        });
        assert!(
            fires,
            "{name} expected to fire one of {acceptable:?}, got contexts: {:?}",
            issues
                .iter()
                .filter_map(|i| i.context.as_ref().map(|c| c.to_string()))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn calque_good_and_solo_fixtures_emit_zero_zy_issues() {
    // Substring-only good/solo fixtures must produce zero ZY issues
    // across ALL detectors — they were designed so no substring-only
    // pattern matches.  Boundary-aware good fixtures intentionally probe
    // the sentence-bounded check (ZY2b vs ZY2a, ZY3b vs ZY3a etc.); a
    // substring-only detector with a looser window may still fire on
    // them, which is correct.  For those we verify only that the named
    // detector does NOT fire.
    let fixtures = read_fixtures(FIXTURE_PREFIX);
    let clean: Vec<_> = fixtures
        .iter()
        .filter(|(n, _)| n.contains("_good_") || n.contains("_solo_"))
        .collect();
    assert!(!clean.is_empty(), "no good/solo fixtures");

    for (name, body) in clean {
        let issues = scan_fixture(body);
        let detector = detector_in_id(name);
        let substring_only_strict = matches!(
            detector,
            Some("ZY1a") | Some("ZY2a") | Some("ZY3a") | Some("ZY4a")
        );
        if substring_only_strict {
            assert!(
                issues.is_empty(),
                "{name} (substring-only good) should produce zero ZY issues, got: {:?}",
                issues
                    .iter()
                    .filter_map(|i| i.context.as_ref().map(|c| c.to_string()))
                    .collect::<Vec<_>>()
            );
        } else if let Some(d) = detector {
            // Boundary-aware fixtures: only the NAMED detector must not fire.
            let fires_self = issues
                .iter()
                .any(|i| i.context.as_ref().is_some_and(|c| c.contains(d)));
            assert!(
                !fires_self,
                "{name} should not fire {d}, got: {:?}",
                issues
                    .iter()
                    .filter_map(|i| i.context.as_ref().map(|c| c.to_string()))
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn calque_zy1_coverage() {
    // Gate: ZY1a fires on calque_superlative_zy1_bad_* (≥3 examples).
    let bads = read_fixtures("calque_superlative_zy1_bad_");
    assert!(
        bads.len() >= 3,
        "ZY1a needs ≥3 bad fixtures, got {}",
        bads.len()
    );

    // Good fixtures include the biographical-noun negative case.
    let goods = read_fixtures("calque_superlative_zy1_good_");
    assert!(
        goods.iter().any(|(_, body)| body.contains("畫家")),
        "ZY1a good fixtures must cover the biographical (畫家) case"
    );
}

#[test]
fn calque_zy2_pattern_coverage() {
    // Gate: ZY2a fires on calque_connective_zy2_bad_* covering all 4 patterns.
    let bads = read_fixtures("calque_connective_zy2_bad_");
    let bodies: String = bads.iter().map(|(_, b)| b.clone()).collect();
    for pattern in ["因為", "雖然", "當", "如果"] {
        assert!(
            bodies.contains(pattern),
            "ZY2a bad fixtures must cover {pattern}"
        );
    }
}

#[test]
fn calque_zy3_three_pairs_covered() {
    let bads = read_fixtures("calque_nominalization_zy3_bad_");
    assert!(bads.len() >= 3, "ZY3a needs ≥3 bad fixtures");
}

#[test]
fn calque_zy4_five_pairs_covered() {
    let bads = read_fixtures("calque_falsefriend_zy4_bad_");
    assert!(bads.len() >= 5, "ZY4a needs ≥5 bad fixtures");
}

#[test]
fn calque_zy1b_density_pairs_present() {
    let bads = read_fixtures("calque_superlative_zy1b_bad_");
    let goods = read_fixtures("calque_superlative_zy1b_good_");
    assert!(!bads.is_empty(), "ZY1b needs ≥1 bad fixture");
    assert!(!goods.is_empty(), "ZY1b needs ≥1 good fixture");
}

#[test]
fn calque_zy2b_sentence_pairs_present() {
    let bads = read_fixtures("calque_connective_zy2b_bad_");
    let goods = read_fixtures("calque_connective_zy2b_good_");
    assert!(!bads.is_empty(), "ZY2b needs ≥1 bad fixture");
    assert!(!goods.is_empty(), "ZY2b needs ≥1 good fixture");
}

#[test]
fn calque_zy3b_chain_pairs_present() {
    let bads = read_fixtures("calque_nominalization_zy3b_bad_");
    let goods = read_fixtures("calque_nominalization_zy3b_good_");
    assert!(!bads.is_empty(), "ZY3b needs ≥1 bad fixture");
    assert!(!goods.is_empty(), "ZY3b needs ≥1 good fixture");
}

#[test]
fn calque_zy5_long_premodifier_pairs_present() {
    let bads = read_fixtures("calque_premodifier_zy5_bad_");
    let goods = read_fixtures("calque_premodifier_zy5_good_");
    assert!(!bads.is_empty(), "ZY5 needs ≥1 bad fixture");
    assert!(
        goods.len() >= 2,
        "ZY5 needs ≥2 good fixtures (native long name + comma-broken span)"
    );
}

#[test]
fn every_fixture_filename_resolves_to_known_detector() {
    // The strict tokenizer is only enforced on `_bad_` fixtures via the
    // unwrap_or_else panic in `calque_bad_fixtures_fire_their_detector`.
    // The good/solo loop branches on `Option<&str>` and silently skips
    // None, which would let a malformed filename drop coverage.  Lock
    // every `calque_*.txt` filename into the strict parser here.
    let fixtures = read_fixtures(FIXTURE_PREFIX);
    assert!(!fixtures.is_empty(), "no calque_* fixtures discovered");
    for (name, _) in &fixtures {
        assert!(
            detector_in_id(name).is_some(),
            "{name} does not parse to a known detector — schema drift?"
        );
    }
}
