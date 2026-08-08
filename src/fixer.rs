// Fix application: apply suggested corrections to source text.
//
// Four tiers (strict superset hierarchy):
//   - None: lint only, no fixes applied.
//   - Orthographic: punctuation, spacing, character forms, case, variant,
//     ellipsis, grammar only.  Lexical term substitutions are skipped.
//   - LexicalSafe: orthographic + deterministic term substitutions
//     (exactly one suggestion, no context_clues).  When --verify
//     calibration has run, issues with anchor_match == Some(false)
//     are skipped; anchor_match == None applies unconditionally.
//   - LexicalContextual: all above + context-clue-gated terms.  For
//     rules with context_clues, apply only when a segmenter confirms
//     enough clue words in surrounding text.  Non-clue lexical issues
//     use the same single-suggestion constraint as LexicalSafe.
//     Anchor rejection (Some(false)) is respected for non-clue issues
//     but overridden for clue-gated issues (segmenter provides
//     independent confirmation).
//
// Fixes are applied in a single forward pass (ascending offset order).

#[cfg(test)]
use std::sync::Arc;

use crate::engine::segment::Segmenter;
use crate::rules::ruleset::{Issue, IssueType, Tier2Outcome};

/// Fix mode controlling which issue types are eligible for automatic correction.
///
/// Each tier is a strict superset: None < Orthographic < LexicalSafe < LexicalContextual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixMode {
    /// Lint only -- no fixes applied.
    None,
    /// Orthographic fixes only: punctuation, spacing, character forms, case,
    /// variant, ellipsis, grammar.  Lexical term substitutions are skipped.
    Orthographic,
    /// Orthographic + deterministic term substitutions (exactly one suggestion,
    /// no context_clues).  Equivalent to old 'safe' mode.
    LexicalSafe,
    /// All above + context-clue-gated terms.  For rules with context_clues,
    /// apply only when segmenter confirms enough clue words nearby.
    LexicalContextual,
}

/// Record of a single fix applied to the text.
#[derive(Debug, Clone)]
pub struct AppliedFix {
    /// Byte offset in the original text where the replacement was written.
    pub offset: usize,
    /// Byte length of the original span that was replaced.
    pub old_len: usize,
    /// The replacement string that was written.
    pub replacement: String,
}

/// Result of applying fixes to text.
#[derive(Debug, Clone)]
pub struct FixResult {
    /// The corrected text.
    pub text: String,
    /// Number of fixes applied.
    pub applied: usize,
    /// Number of issues skipped (ineligible for the chosen fix tier, or in excluded regions).
    pub skipped: usize,
    /// Detailed record of each applied fix, stored in ascending offset
    /// order (forward pass). Used for position-based convergence
    /// suppression and exact offset remapping after re-scan.
    pub applied_fixes: Vec<AppliedFix>,
}

/// Minimum context clue words for aggressive fixer: confusable rules need
/// higher confidence (2 clues) because both forms are valid in different
/// contexts. Cross-strait and other rule types need only 1 clue because
/// the match itself is already a strong signal of incorrect regional usage.
const MIN_CLUE_MATCHES_CONFUSABLE: usize = 2;
const MIN_CLUE_MATCHES_DEFAULT: usize = 1;

/// Apply fixes to text based on the given issues.
///
/// Convenience wrapper that calls [apply_fixes_with_context] without a
/// segmenter.  Context-clue-dependent rules are treated as ambiguous.
pub fn apply_fixes(
    text: &str,
    issues: &[Issue],
    mode: FixMode,
    excluded_offsets: &[(usize, usize)],
) -> FixResult {
    apply_fixes_with_context(text, issues, mode, excluded_offsets, None)
}

/// Apply fixes to text using an optional segmenter for context-clue analysis.
///
/// Issues must be sorted by offset (ascending) and non-overlapping
/// (guaranteed by the scanner's resolve_overlaps pass).  Fixes are
/// applied in a single forward pass (ascending offset order): chunks of
/// unchanged text are copied between replacement spans, yielding O(N).
///
/// Fix tiers control which issues are eligible:
///   - Orthographic: only Punctuation/Case/Variant/Grammar issues.
///   - LexicalSafe: above + lexical issues without context_clues,
///     single suggestion only.  When `--verify` calibration has run,
///     issues with `anchor_match == Some(false)` are skipped (calibration
///     rejected the term).  `anchor_match == None` (no calibration)
///     applies unconditionally.
///   - LexicalContextual: all above + context-clue-gated lexical issues,
///     verified by segmenter when available.  For non-clue issues, respects
///     anchor rejections (no independent disambiguation).  For clue-gated
///     issues, the segmenter overrides anchor rejection.
pub fn apply_fixes_with_context(
    text: &str,
    issues: &[Issue],
    mode: FixMode,
    excluded_offsets: &[(usize, usize)],
    segmenter: Option<&Segmenter>,
) -> FixResult {
    let started = std::time::Instant::now();
    let _span = tracing::info_span!(
        "fix",
        content_length = text.len() as u64,
        issue_count = issues.len() as u64,
        mode = ?mode
    )
    .entered();
    // Lint-only mode: no fixes attempted, nothing to skip.
    if mode == FixMode::None {
        tracing::info!(
            fix_count = 0_u64,
            skipped_count = 0_u64,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "fix completed"
        );
        return FixResult {
            text: text.to_string(),
            applied: 0,
            skipped: 0,
            applied_fixes: Vec::new(),
        };
    }

    let mut out = String::with_capacity(text.len());
    let mut applied = 0usize;
    let mut skipped = 0usize;
    let mut applied_fixes = Vec::new();
    // Byte position up to which we have already copied into `out`.
    let mut cursor: usize = 0;

    // Pre-compute excluded ranges for context-clue window lookups (hoisted
    // out of the per-issue loop to avoid redundant allocations).
    let excluded_ranges: Vec<crate::engine::excluded::ByteRange> = excluded_offsets
        .iter()
        .map(|&(start, end)| crate::engine::excluded::ByteRange { start, end })
        .collect();

    // Issues are already sorted ascending by offset and non-overlapping
    // (scanner's resolve_overlaps guarantees this).  Iterate forward,
    // copying unchanged gaps and appending replacements.
    for issue in issues {
        let Some(end) = issue.offset.checked_add(issue.length) else {
            tracing::warn!(
                "skipping malformed issue at offset {}: length overflow",
                issue.offset
            );
            skipped += 1;
            continue;
        };

        // Skip overlapping issues: grammar issues are appended after
        // overlap resolution and may overlap each other (e.g. 對X進行Y
        // overlaps the inner 進行Y).  The fixer must not apply both.
        if issue.offset < cursor {
            skipped += 1;
            continue;
        }

        // Skip if the issue span overlaps any excluded region.
        if excluded_offsets
            .iter()
            .any(|&(s, e)| issue.offset < e && end > s)
        {
            skipped += 1;
            continue;
        }

        // Tier-based fix eligibility.
        //
        // Orthographic issue types can be fixed mechanically (no lexical
        // ambiguity).  Lexical types (CrossStrait, Typo, PoliticalColoring,
        // Confusable) need progressively higher fix tiers.
        // AiStyle zero-width artifact removal (empty suggestion on invisible
        // chars only) is safe for orthographic tier -- deletes invisible junk.
        // The found-content check prevents future AiStyle rules with empty
        // suggestions from being misclassified as orthographic.
        // Narrower check than ai_score::is_zero_width: only ZWSP (U+200B)
        // and mid-text BOM (U+FEFF) are pure tokenizer junk safe to strip
        // unconditionally. ZWJ/ZWNJ/LRM/RLM have legitimate uses in bidi
        // text and emoji sequences; the broader set in ai_score.rs is
        // appropriate for detection/scoring but too aggressive for fixing.
        let ai_zero_width_removal = issue.rule_type == IssueType::AiStyle
            && crate::rules::ruleset::is_delete_suggestion(&issue.suggestions)
            && !issue.found.is_empty()
            && issue.found.chars().all(|ch| {
                ch == '\u{200B}' || (ch == '\u{FEFF}' && issue.offset > 0) // preserve file-start BOM
            });
        let orthographic = matches!(
            issue.rule_type,
            IssueType::Punctuation | IssueType::Case | IssueType::Variant | IssueType::Grammar
        ) || ai_zero_width_removal;

        // Orthographic tier: skip all lexical issues.
        if mode == FixMode::Orthographic && !orthographic {
            skipped += 1;
            continue;
        }

        // Tier 2 can suppress lexical issues as likely false positives.
        // Respect that suppression during auto-fix so we do not rewrite
        // general prose like "學習的進程" into OS terminology.
        // An external resolver has already selected an allowlisted candidate
        // for this exact span, so its decision is allowed to override the
        // local gray-zone/suppressed outcome.
        let externally_resolved = issue.llm_judged && issue.suggestions.len() == 1;
        if !orthographic && !externally_resolved && issue.tier2_outcome == Tier2Outcome::Suppressed
        {
            skipped += 1;
            continue;
        }

        // Pre-compute context-clue presence for gating decisions below.
        let has_clues = issue.context_clues.as_ref().is_some_and(|c| !c.is_empty());

        // Anchor-match gating for lexical issues: when calibration has
        // run (--verify), anchor_match carries the verdict.  If calibration
        // explicitly rejected the term (Some(false)), skip the fix —
        // both LexicalSafe and LexicalContextual respect anchor rejection
        // for non-clue issues (no independent disambiguation available).
        // Context-clue-gated issues in LexicalContextual can override
        // rejection because the segmenter provides independent confirmation.
        // When anchor_match is None (no calibration), apply unconditionally.
        if !orthographic && issue.anchor_match == Some(false) && !has_clues {
            skipped += 1;
            continue;
        }

        // Context-clue gating for lexical issues.
        if has_clues && !orthographic && !externally_resolved {
            // Only LexicalContextual can handle context-clue-gated terms.
            if mode != FixMode::LexicalContextual {
                skipped += 1;
                continue;
            }
            // Threshold is type-aware: confusable rules (both forms valid in
            // different contexts) need 2 clues for confidence; cross-strait and
            // other rules need only 1 (the match itself is a strong regional
            // signal, one nearby clue is sufficient to confirm domain).
            let min_clues = if issue.rule_type == IssueType::Confusable {
                MIN_CLUE_MATCHES_CONFUSABLE
            } else {
                MIN_CLUE_MATCHES_DEFAULT
            };
            let confirmed = segmenter.is_some_and(|seg| {
                let window = crate::engine::scan::surrounding_window_bounded(
                    text,
                    issue.offset,
                    end,
                    &excluded_ranges,
                );
                let clue_strs: Vec<&str> = issue
                    .context_clues
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str())
                    .collect();
                seg.count_context_clues(window, &clue_strs) >= min_clues
            });
            if !confirmed {
                skipped += 1;
                continue;
            }
        }

        // Suggestion selection.
        //   - Orthographic issues: always pick first suggestion (mechanical,
        //     no lexical ambiguity).
        //   - Lexical issues (both clue-gated and non-clue): single suggestion
        //     only.  The segmenter confirms domain context but does not
        //     disambiguate between multiple replacement candidates.
        let rep = match mode {
            _ if orthographic => issue.suggestions.first(),
            _ if issue.suggestions.len() == 1 => Some(&issue.suggestions[0]),
            _ => None,
        };
        let Some(rep) = rep.filter(|_| end <= text.len()) else {
            skipped += 1;
            continue;
        };

        out.push_str(&text[cursor..issue.offset]);
        out.push_str(rep);
        cursor = end;
        applied_fixes.push(AppliedFix {
            offset: issue.offset,
            old_len: issue.length,
            replacement: rep.clone(),
        });
        applied += 1;
    }

    // Copy the remaining tail after the last fix (or the entire text if
    // no fixes were applied).
    out.push_str(&text[cursor..]);

    tracing::info!(
        fix_count = applied as u64,
        skipped_count = skipped as u64,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "fix completed"
    );
    FixResult {
        text: out,
        applied,
        skipped,
        applied_fixes,
    }
}

/// Map an original-text byte offset to its position in the fixed text.
///
/// Accumulates byte deltas (replacement.len() - old_len) from all applied
/// fixes whose original offset is strictly before orig_offset.  All fix
/// offsets are in original-text coordinates and non-overlapping.
pub fn remap_to_post_fix(orig_offset: usize, applied_fixes: &[AppliedFix]) -> usize {
    let mut delta: isize = 0;
    for fix in applied_fixes {
        if fix.offset < orig_offset {
            delta += fix.replacement.len() as isize - fix.old_len as isize;
        }
    }
    let result = orig_offset as isize + delta;
    debug_assert!(result >= 0, "remap produced negative offset");
    result.max(0) as usize
}

/// Remap exclusion zones from original-text coordinates to post-fix coordinates.
///
/// The fixer never applies fixes inside excluded regions, so exclusion zones
/// remain structurally intact -- only their byte offsets shift due to
/// earlier replacements having different lengths than the originals.
///
/// Uses a merge-style single forward pass over both sorted sequences
/// (applied_fixes and exclusions), accumulating deltas in O(E + F) time.
pub fn remap_exclusions(
    exclusions: &[crate::engine::excluded::ByteRange],
    applied_fixes: &[AppliedFix],
) -> Vec<crate::engine::excluded::ByteRange> {
    use crate::engine::excluded::ByteRange;

    if applied_fixes.is_empty() {
        return exclusions.to_vec();
    }

    let mut delta: isize = 0;
    let mut fix_idx = 0;
    exclusions
        .iter()
        .map(|&ByteRange { start, end }| {
            // Advance past all fixes whose span ends at or before this
            // exclusion zone.  The end-of-span check (offset + old_len)
            // is critical for zero-length insertions (e.g. spacing fixes
            // with old_len == 0): an insertion at the exclusion boundary
            // must shift the zone right.
            while fix_idx < applied_fixes.len() {
                let fix = &applied_fixes[fix_idx];
                let fix_end = fix.offset.saturating_add(fix.old_len);
                if fix_end > start {
                    break;
                }
                delta += fix.replacement.len() as isize - fix.old_len as isize;
                fix_idx += 1;
            }
            let new_start = (start as isize + delta).max(0) as usize;
            let new_end = (end as isize + delta).max(0) as usize;
            ByteRange {
                start: new_start,
                end: new_end,
            }
        })
        .collect()
}

/// Remove re-scan issues whose byte range overlaps a region written by the fixer.
///
/// After applying fixes and re-scanning, the fixer may have introduced new
/// text that triggers rules (convergent chain).  These are noise: the fixer
/// already chose the best replacement.  This function suppresses them by
/// checking each re-scan issue against the post-fix byte ranges of applied
/// fixes.
pub fn suppress_convergent_issues(issues: &mut Vec<Issue>, applied_fixes: &[AppliedFix]) {
    if applied_fixes.is_empty() {
        return;
    }
    // Build post-fix ranges in a single forward pass (O(n)) instead of
    // calling remap_to_post_fix per fix (O(n) each, O(n^2) total).
    // Applied fixes are sorted by offset and non-overlapping, so a running
    // delta accumulator gives the correct remapped position for each fix.
    let mut delta: isize = 0;
    let fix_ranges: Vec<(usize, usize)> = applied_fixes
        .iter()
        .map(|fix| {
            let post = (fix.offset as isize + delta).max(0) as usize;
            delta += fix.replacement.len() as isize - fix.old_len as isize;
            (post, post + fix.replacement.len())
        })
        .collect();
    issues.retain(|issue| {
        let issue_end = issue.offset + issue.length;
        !fix_ranges.iter().any(|&(start, end)| {
            if start == end {
                // Zero-length deletion: suppress issues touching this offset.
                issue.offset <= start && issue_end > start
            } else {
                issue.offset < end && issue_end > start
            }
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::scan::surrounding_window;
    use crate::rules::ruleset::{IssueType, Severity};

    fn make_issue(offset: usize, found: &str, suggestions: Vec<&str>) -> Issue {
        Issue::new(
            offset,
            found.len(),
            found,
            suggestions.into_iter().map(String::from).collect(),
            IssueType::CrossStrait,
            Severity::Warning,
        )
    }

    fn make_issue_with_clues(
        offset: usize,
        found: &str,
        suggestions: Vec<&str>,
        clues: Vec<&str>,
    ) -> Issue {
        Issue::new(
            offset,
            found.len(),
            found,
            suggestions.into_iter().map(String::from).collect(),
            IssueType::Confusable,
            Severity::Warning,
        )
        .with_english("program")
        .with_context_clues(clues.into_iter().map(String::from).collect())
    }

    fn make_punctuation_issue(offset: usize, found: &str, suggestions: Vec<&str>) -> Issue {
        Issue::new(
            offset,
            found.len(),
            found,
            suggestions.into_iter().map(String::from).collect(),
            IssueType::Punctuation,
            Severity::Warning,
        )
    }

    #[test]
    fn lexical_safe_single_suggestion() {
        let text = "這個軟件很好用";
        let issues = vec![make_issue(6, "軟件", vec!["軟體"])];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "這個軟體很好用");
        assert_eq!(result.applied, 1);
        assert_eq!(result.skipped, 0);
    }

    #[test]
    fn lexical_safe_multiple_suggestions_skipped() {
        let text = "這個視頻很好看";
        let issues = vec![make_issue(6, "視頻", vec!["影片", "影音"])];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, text); // unchanged
        assert_eq!(result.applied, 0);
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_contextual_skips_multi_suggestion_non_clue() {
        // Multi-suggestion lexical issue without context_clues: both
        // LexicalSafe and LexicalContextual skip it (no disambiguation).
        let text = "這個視頻很好看";
        let issues = vec![make_issue(6, "視頻", vec!["影片", "影音"])];
        let result = apply_fixes(text, &issues, FixMode::LexicalContextual, &[]);
        assert_eq!(result.text, text); // unchanged -- ambiguous, no clues
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn multiple_fixes() {
        let text = "這個軟件的內存";
        let issues = vec![
            make_issue(6, "軟件", vec!["軟體"]),
            make_issue(15, "內存", vec!["記憶體"]),
        ];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "這個軟體的記憶體");
        assert_eq!(result.applied, 2);
    }

    #[test]
    fn excluded_offset_skipped() {
        let text = "這個軟件很好用";
        let issues = vec![make_issue(6, "軟件", vec!["軟體"])];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[(0, 21)]);
        assert_eq!(result.text, text);
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn empty_issues() {
        let text = "hello";
        let result = apply_fixes(text, &[], FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "hello");
        assert_eq!(result.applied, 0);
    }

    // -- Orthographic tier tests --

    #[test]
    fn orthographic_fixes_punctuation() {
        let text = "你好,世界";
        let issues = vec![make_punctuation_issue(6, ",", vec!["，"])];
        let result = apply_fixes(text, &issues, FixMode::Orthographic, &[]);
        assert_eq!(result.text, "你好，世界");
        assert_eq!(result.applied, 1);
    }

    #[test]
    fn orthographic_skips_lexical_issues() {
        let text = "這個軟件很好用";
        let issues = vec![make_issue(6, "軟件", vec!["軟體"])];
        let result = apply_fixes(text, &issues, FixMode::Orthographic, &[]);
        assert_eq!(result.text, text); // unchanged -- orthographic skips CrossStrait
        assert_eq!(result.skipped, 1);
    }

    // -- Anchor-match gating tests --

    #[test]
    fn lexical_safe_skips_anchor_rejected() {
        let text = "這個軟件很好用";
        let mut issue = make_issue(6, "軟件", vec!["軟體"]);
        issue.anchor_match = Some(false); // calibration rejected
        let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, text); // unchanged -- anchor rejected
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_safe_applies_anchor_confirmed() {
        let text = "這個軟件很好用";
        let mut issue = make_issue(6, "軟件", vec!["軟體"]);
        issue.anchor_match = Some(true); // calibration confirmed
        let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "這個軟體很好用");
        assert_eq!(result.applied, 1);
    }

    #[test]
    fn lexical_safe_applies_anchor_none() {
        let text = "這個軟件很好用";
        let issue = make_issue(6, "軟件", vec!["軟體"]);
        // anchor_match == None (no calibration) -- should apply unconditionally
        assert!(issue.anchor_match.is_none());
        let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "這個軟體很好用");
        assert_eq!(result.applied, 1);
    }

    #[test]
    fn lexical_contextual_respects_anchor_rejection_for_non_clue() {
        // Non-clue lexical issue with anchor rejection: LexicalContextual
        // respects it because there is no independent disambiguation signal.
        let text = "這個軟件很好用";
        let mut issue = make_issue(6, "軟件", vec!["軟體"]);
        issue.anchor_match = Some(false);
        let result = apply_fixes(text, &[issue], FixMode::LexicalContextual, &[]);
        assert_eq!(result.text, text); // unchanged -- anchor rejected, no clues
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_contextual_skips_tier2_suppressed_issue() {
        let text = "學習的進程需要耐心和毅力";
        let offset = text.find("進程").unwrap();
        let mut issue = make_issue(offset, "進程", vec!["行程"]);
        issue.tier2_outcome = Tier2Outcome::Suppressed;
        issue.severity = Severity::Info;
        let result = apply_fixes(text, &[issue], FixMode::LexicalContextual, &[]);
        assert_eq!(result.text, text);
        assert_eq!(result.skipped, 1);
    }

    // -- Combined anchor_match + context_clues tests --

    #[test]
    fn lexical_safe_skips_clue_rule_even_with_anchor_confirmed() {
        // anchor_match == Some(true) but has context_clues → LexicalSafe
        // still refuses because context-clue rules need LexicalContextual.
        let text = "我需要編寫一個程序來執行";
        let offset = text.find("程序").unwrap();
        let mut issue = make_issue_with_clues(
            offset,
            "程序",
            vec!["程式"],
            vec!["編寫", "代碼", "執行", "開發"],
        );
        issue.anchor_match = Some(true);
        let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, text); // unchanged -- context_clues gate takes precedence
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_contextual_applies_clue_rule_despite_anchor_rejection() {
        // anchor_match == Some(false) + context_clues present.
        // LexicalContextual overrides anchor rejection and applies if
        // segmenter confirms clues.
        let text = "我需要編寫一個程序來執行";
        let offset = text.find("程序").unwrap();
        let mut issue = make_issue_with_clues(
            offset,
            "程序",
            vec!["程式"],
            vec!["編寫", "代碼", "執行", "開發"],
        );
        issue.anchor_match = Some(false);
        let seg = Segmenter::new(
            ["編寫", "代碼", "執行", "開發", "程序", "程式"]
                .iter()
                .map(|s| s.to_string()),
        );
        let result =
            apply_fixes_with_context(text, &[issue], FixMode::LexicalContextual, &[], Some(&seg));
        assert_eq!(result.text, "我需要編寫一個程式來執行");
        assert_eq!(result.applied, 1);
    }

    // -- Context clue tests --

    #[test]
    fn lexical_safe_skips_issues_with_context_clues() {
        let text = "我需要編寫一個程序來執行";
        let offset = text.find("程序").unwrap();
        let issues = vec![make_issue_with_clues(
            offset,
            "程序",
            vec!["程式"],
            vec!["編寫", "代碼", "執行", "開發"],
        )];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, text); // unchanged -- lexical_safe refuses context-clue rules
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_contextual_with_segmenter_applies_when_clues_match() {
        let text = "我需要編寫一個程序來執行";
        let offset = text.find("程序").unwrap();
        let issues = vec![make_issue_with_clues(
            offset,
            "程序",
            vec!["程式"],
            vec!["編寫", "代碼", "執行", "開發"],
        )];
        let seg = Segmenter::new(
            ["編寫", "代碼", "執行", "開發", "程序", "程式"]
                .iter()
                .map(|s| s.to_string()),
        );
        let result =
            apply_fixes_with_context(text, &issues, FixMode::LexicalContextual, &[], Some(&seg));
        assert_eq!(result.text, "我需要編寫一個程式來執行");
        assert_eq!(result.applied, 1);
    }

    #[test]
    fn lexical_contextual_with_segmenter_skips_when_clues_insufficient() {
        let text = "這個程序很重要";
        let offset = text.find("程序").unwrap();
        let issues = vec![make_issue_with_clues(
            offset,
            "程序",
            vec!["程式"],
            vec!["編寫", "代碼", "執行", "開發"],
        )];
        let seg = Segmenter::new(
            ["編寫", "代碼", "執行", "開發", "程序", "程式"]
                .iter()
                .map(|s| s.to_string()),
        );
        let result =
            apply_fixes_with_context(text, &issues, FixMode::LexicalContextual, &[], Some(&seg));
        assert_eq!(result.text, text); // unchanged -- insufficient clues
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_contextual_without_segmenter_skips_clue_rules() {
        let text = "這個程序很重要";
        let offset = text.find("程序").unwrap();
        let issues = vec![make_issue_with_clues(
            offset,
            "程序",
            vec!["程式"],
            vec!["編寫", "代碼", "執行", "開發"],
        )];
        let result = apply_fixes(text, &issues, FixMode::LexicalContextual, &[]);
        assert_eq!(result.text, text); // unchanged -- no segmenter, cannot verify clues
        assert_eq!(result.skipped, 1);
    }

    // -- AiStyle tier exclusion tests --

    fn make_ai_style_issue(offset: usize, found: &str, suggestions: Vec<&str>) -> Issue {
        Issue::new(
            offset,
            found.len(),
            found,
            suggestions.into_iter().map(String::from).collect(),
            IssueType::AiStyle,
            Severity::Info,
        )
    }

    #[test]
    fn orthographic_skips_ai_style_issues() {
        let text = "這個系統作為核心元件";
        let offset = text.find("作為").unwrap();
        let issues = vec![make_ai_style_issue(offset, "作為", vec!["是"])];
        let result = apply_fixes(text, &issues, FixMode::Orthographic, &[]);
        assert_eq!(result.text, text); // unchanged — AiStyle not orthographic
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn lexical_safe_applies_single_suggestion_ai_style() {
        // Semantic safety words (意味著→表示) have a single suggestion
        // and are eligible for lexical_safe auto-fix.
        let text = "這個定義意味著所有值";
        let offset = text.find("意味著").unwrap();
        let issues = vec![make_ai_style_issue(offset, "意味著", vec!["表示"])];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "這個定義表示所有值");
        assert_eq!(result.applied, 1);
    }

    #[test]
    fn lexical_safe_skips_ai_style_no_suggestions() {
        let text = "這意味著很多事情";
        let offset = text.find("意味著").unwrap();
        let issues = vec![make_ai_style_issue(offset, "意味著", vec![])];
        let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, text); // unchanged — no suggestion
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn surrounding_window_basic() {
        let text = "AABBCCDDEE";
        let window = surrounding_window(text, 4, 6);
        // Window should include chars around the CC range
        assert!(window.contains('A'));
        assert!(window.contains('E'));
    }

    #[test]
    fn surrounding_window_cjk() {
        let text = "我需要編寫一個程序來執行這個任務";
        let offset = text.find("程序").unwrap();
        let end = offset + "程序".len();
        let window = surrounding_window(text, offset, end);
        assert!(window.contains("編寫"));
        assert!(window.contains("執行"));
    }

    #[test]
    fn surrounding_window_empty_text() {
        let window = surrounding_window("", 0, 0);
        assert_eq!(window, "");
    }

    #[test]
    fn surrounding_window_at_boundaries() {
        // Match spans entire text -- window should return the whole string.
        let text = "程序";
        let window = surrounding_window(text, 0, text.len());
        assert_eq!(window, "程序");
    }

    // -- suppress_convergent_issues O(n) equivalence tests --

    #[test]
    fn suppress_convergent_o_n_matches_o_n2() {
        // Verify the O(n) forward-pass remap produces identical fix_ranges
        // to the old per-fix remap_to_post_fix approach.
        let cases: Vec<Vec<AppliedFix>> = vec![
            // Empty
            vec![],
            // Single fix, same length (no shift)
            vec![AppliedFix {
                offset: 6,
                old_len: 6,
                replacement: "軟體".into(),
            }],
            // Single fix, expansion (6 bytes -> 9 bytes)
            vec![AppliedFix {
                offset: 6,
                old_len: 6,
                replacement: "記憶體".into(),
            }],
            // Single fix, contraction (9 bytes -> 6 bytes)
            vec![AppliedFix {
                offset: 6,
                old_len: 9,
                replacement: "軟體".into(),
            }],
            // Single fix, deletion (6 bytes -> 0 bytes)
            vec![AppliedFix {
                offset: 6,
                old_len: 6,
                replacement: String::new(),
            }],
            // Two fixes, both same length
            vec![
                AppliedFix {
                    offset: 6,
                    old_len: 6,
                    replacement: "軟體".into(),
                },
                AppliedFix {
                    offset: 15,
                    old_len: 6,
                    replacement: "記憶".into(),
                },
            ],
            // Two fixes, first expands
            vec![
                AppliedFix {
                    offset: 6,
                    old_len: 6,
                    replacement: "記憶體".into(),
                },
                AppliedFix {
                    offset: 15,
                    old_len: 6,
                    replacement: "軟體".into(),
                },
            ],
            // Two fixes, first contracts
            vec![
                AppliedFix {
                    offset: 6,
                    old_len: 9,
                    replacement: "AB".into(),
                },
                AppliedFix {
                    offset: 20,
                    old_len: 6,
                    replacement: "CD".into(),
                },
            ],
            // Two fixes, first is deletion
            vec![
                AppliedFix {
                    offset: 6,
                    old_len: 6,
                    replacement: String::new(),
                },
                AppliedFix {
                    offset: 15,
                    old_len: 6,
                    replacement: "XY".into(),
                },
            ],
            // Three fixes with mixed shifts
            vec![
                AppliedFix {
                    offset: 0,
                    old_len: 3,
                    replacement: "ABCDE".into(),
                },
                AppliedFix {
                    offset: 10,
                    old_len: 6,
                    replacement: "X".into(),
                },
                AppliedFix {
                    offset: 20,
                    old_len: 3,
                    replacement: "YZW".into(),
                },
            ],
        ];

        for (i, fixes) in cases.iter().enumerate() {
            // O(n^2) reference: call remap_to_post_fix per fix
            let expected: Vec<(usize, usize)> = fixes
                .iter()
                .map(|fix| {
                    let post = remap_to_post_fix(fix.offset, fixes);
                    (post, post + fix.replacement.len())
                })
                .collect();

            // O(n) forward pass
            let mut delta: isize = 0;
            let actual: Vec<(usize, usize)> = fixes
                .iter()
                .map(|fix| {
                    let post = (fix.offset as isize + delta).max(0) as usize;
                    delta += fix.replacement.len() as isize - fix.old_len as isize;
                    (post, post + fix.replacement.len())
                })
                .collect();

            assert_eq!(expected, actual, "case {i} mismatch: fixes={fixes:?}");
        }
    }

    #[test]
    fn suppress_convergent_deletion_suppresses_touching_issue() {
        // A deletion (replacement is empty) should suppress issues that
        // touch the deletion point.
        let fixes = vec![AppliedFix {
            offset: 6,
            old_len: 6,
            replacement: String::new(),
        }];
        // Issue at post-fix offset 6 (the deletion point) should be suppressed.
        let mut issues = vec![make_issue(6, "XX", vec!["YY"])];
        suppress_convergent_issues(&mut issues, &fixes);
        assert!(
            issues.is_empty(),
            "issue touching deletion point should be suppressed"
        );
    }

    #[test]
    fn suppress_convergent_preserves_non_overlapping_issue() {
        let fixes = vec![AppliedFix {
            offset: 6,
            old_len: 6,
            replacement: "軟體".into(),
        }];
        // Issue at offset 20, well past the fix range -- should survive.
        let mut issues = vec![make_issue(20, "內存", vec!["記憶體"])];
        suppress_convergent_issues(&mut issues, &fixes);
        assert_eq!(issues.len(), 1, "non-overlapping issue should be preserved");
    }

    #[test]
    fn empty_context_clues_vec_treated_as_no_clues() {
        // Issue with context_clues: Some(vec![]) should NOT be skipped in
        // lexical_safe because the empty vec means no ambiguity.
        let text = "這個軟件很好用";
        let mut issue = make_issue(6, "軟件", vec!["軟體"]);
        issue.context_clues = Some(Arc::from(Vec::<String>::new()));
        let result = apply_fixes(text, &[issue], FixMode::LexicalSafe, &[]);
        assert_eq!(result.text, "這個軟體很好用");
        assert_eq!(result.applied, 1);
    }

    // --- remap_exclusions tests ---

    use crate::engine::excluded::ByteRange;

    fn br(start: usize, end: usize) -> ByteRange {
        ByteRange { start, end }
    }

    #[test]
    fn remap_exclusions_no_fixes() {
        let excl = vec![br(10, 20), br(30, 40)];
        let result = remap_exclusions(&excl, &[]);
        assert_eq!(result, vec![br(10, 20), br(30, 40)]);
    }

    #[test]
    fn remap_exclusions_fix_before_exclusion_grows() {
        // Fix at offset 5 replaces 2 bytes with 4 bytes (+2 delta).
        // Exclusion at (10, 20) should shift to (12, 22).
        let excl = vec![br(10, 20)];
        let fixes = vec![AppliedFix {
            offset: 5,
            old_len: 2,
            replacement: "abcd".to_string(),
        }];
        let result = remap_exclusions(&excl, &fixes);
        assert_eq!(result, vec![br(12, 22)]);
    }

    #[test]
    fn remap_exclusions_fix_before_exclusion_shrinks() {
        // Fix at offset 2 replaces 4 bytes with 1 byte (-3 delta).
        // Exclusion at (10, 20) should shift to (7, 17).
        let excl = vec![br(10, 20)];
        let fixes = vec![AppliedFix {
            offset: 2,
            old_len: 4,
            replacement: "x".to_string(),
        }];
        let result = remap_exclusions(&excl, &fixes);
        assert_eq!(result, vec![br(7, 17)]);
    }

    #[test]
    fn remap_exclusions_fix_after_exclusion() {
        // Fix at offset 25 is after the exclusion at (10, 20) -- no shift.
        let excl = vec![br(10, 20)];
        let fixes = vec![AppliedFix {
            offset: 25,
            old_len: 3,
            replacement: "abcdef".to_string(),
        }];
        let result = remap_exclusions(&excl, &fixes);
        assert_eq!(result, vec![br(10, 20)]);
    }

    #[test]
    fn remap_exclusions_multiple_fixes_multiple_zones() {
        // Fix at 5: 2->4 (+2), fix at 25: 3->1 (-2).
        // Exclusion (10,20) shifts by +2 -> (12,22).
        // Exclusion (30,40) shifts by +2-2=0 -> (30,40).
        let excl = vec![br(10, 20), br(30, 40)];
        let fixes = vec![
            AppliedFix {
                offset: 5,
                old_len: 2,
                replacement: "abcd".to_string(),
            },
            AppliedFix {
                offset: 25,
                old_len: 3,
                replacement: "x".to_string(),
            },
        ];
        let result = remap_exclusions(&excl, &fixes);
        assert_eq!(result, vec![br(12, 22), br(30, 40)]);
    }

    #[test]
    fn remap_exclusions_zero_length_insertion_at_boundary() {
        // Spacing fix: zero-length insertion (old_len=0) at offset 10,
        // which is exactly the exclusion start. The insertion should
        // shift the exclusion right by the replacement length.
        let excl = vec![br(10, 20)];
        let fixes = vec![AppliedFix {
            offset: 10,
            old_len: 0,
            replacement: " ".to_string(),
        }];
        let result = remap_exclusions(&excl, &fixes);
        assert_eq!(result, vec![br(11, 21)]);
    }
}
