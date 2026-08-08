// 35.9 — project glossary integration test.
//
// Verifies that `[glossary] banned`, `proper_nouns`, and `preferred`
// fields in `.zhtw-mcp.toml` are honored by the `lint` subcommand.

use std::process::{Command, Stdio};

fn binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("zhtw-mcp");
    path
}

#[test]
fn glossary_banned_term_fires_even_without_context_clues() {
    let dir = tempfile::tempdir().unwrap();
    // 線程 (mainland) — embedded ruleset already flags this, but we want
    // to confirm the glossary-driven path also works.  We use a banned
    // term that the embedded ruleset would NOT flag in isolation: the
    // word "ABC" with banned=["ABC"] forces flagging.
    std::fs::write(
        dir.path().join(".zhtw-mcp.toml"),
        "[glossary]\nbanned = [\"ABC\"]\n",
    )
    .unwrap();
    let md = dir.path().join("test.md");
    std::fs::write(&md, "ABC 不該出現在文件中。\n").unwrap();

    let bin = binary_path();
    let output = Command::new(&bin)
        .args(["lint", md.to_str().unwrap(), "--format", "json"])
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let issues = parsed["issues"].as_array().expect("issues array");
    assert!(
        issues.iter().any(|i| i["found"] == "ABC"),
        "banned ABC must produce a synthetic issue; got {issues:?}"
    );
}

#[test]
fn glossary_proper_noun_suppresses_matching_issue() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".zhtw-mcp.toml"),
        "[glossary]\nproper_nouns = [\"線程\"]\n",
    )
    .unwrap();
    let md = dir.path().join("test.md");
    std::fs::write(&md, "我們的 線程 實作。\n").unwrap();

    let bin = binary_path();
    let output = Command::new(&bin)
        .args(["lint", md.to_str().unwrap(), "--format", "json"])
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let issues = parsed["issues"].as_array().expect("issues array");
    assert!(
        !issues.iter().any(|i| i["found"] == "線程"),
        "proper_nouns must suppress 線程; got {issues:?}"
    );
}

#[test]
fn glossary_banned_does_not_duplicate_existing_issues() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".zhtw-mcp.toml"),
        "[glossary]\nbanned = [\"線程\"]\n",
    )
    .unwrap();
    let md = dir.path().join("test.md");
    std::fs::write(&md, "我們的線程實作。\n").unwrap();

    let bin = binary_path();
    let output = Command::new(&bin)
        .args(["lint", md.to_str().unwrap(), "--format", "json"])
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let issues = parsed["issues"].as_array().expect("issues array");
    let line_hits: Vec<_> = issues.iter().filter(|i| i["found"] == "線程").collect();
    assert_eq!(
        line_hits.len(),
        1,
        "banned 線程 must not duplicate the embedded rule's hit; got {line_hits:?}"
    );
}

#[test]
fn glossary_banned_is_honored_during_fix_runs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".zhtw-mcp.toml"),
        "[glossary]\nbanned = [\"ABC\"]\n",
    )
    .unwrap();
    let md = dir.path().join("test.md");
    std::fs::write(&md, "ABC 不該出現在文件中。\n").unwrap();

    let bin = binary_path();
    let output = Command::new(&bin)
        .args(["lint", md.to_str().unwrap(), "--format", "json", "--fix"])
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let issues = parsed["issues"].as_array().expect("issues array");
    assert!(
        issues.iter().any(|i| i["found"] == "ABC"),
        "glossary banned terms must remain active during fix runs; got {issues:?}"
    );
}

#[test]
fn glossary_proper_nouns_are_honored_during_fix_runs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".zhtw-mcp.toml"),
        "[glossary]\nproper_nouns = [\"線程\"]\n",
    )
    .unwrap();
    let md = dir.path().join("test.md");
    std::fs::write(&md, "我們的線程實作。\n").unwrap();

    let bin = binary_path();
    let output = Command::new(&bin)
        .args(["lint", md.to_str().unwrap(), "--format", "json", "--fix"])
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let issues = parsed["issues"].as_array().expect("issues array");
    assert!(
        !issues.iter().any(|i| i["found"] == "線程"),
        "glossary proper_nouns must suppress fix-path issues; got {issues:?}"
    );
}
