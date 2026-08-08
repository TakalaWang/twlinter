// Document-level AI signature scoring.
//
// Aggregates per-occurrence issues, density data, and structural pattern counts
// into a single composite score.  Deterministic, no ML.
//
// The score is a weighted sum of normalized density ratios and structural
// indicators.  Each signal contributes proportionally to how far it exceeds
// its human baseline threshold.

use serde::{Deserialize, Serialize};

use crate::engine::excluded::{is_excluded, ByteRange};
use crate::rules::ruleset::{Issue, IssueType};

// Density thresholds: (phrase, human_baseline, threshold, weight).
// Weight controls contribution to the composite score.
const DENSITY_SIGNALS: &[(&str, f32, f32, f32)] = &[
    ("更重要的是", 0.3, 0.5, 1.0),
    ("值得注意的是", 0.2, 0.3, 1.0),
    ("這意味著", 0.3, 0.5, 0.8),
    ("不容忽視", 0.1, 0.2, 0.7),
    ("深刻影響", 0.2, 0.3, 0.8),
    ("從某種意義上", 0.1, 0.2, 0.6),
    ("從某種程度上", 0.1, 0.2, 0.6),
    ("需要注意的是", 0.2, 0.3, 0.8),
    ("在某種程度上", 0.1, 0.2, 0.6),
    ("在這個過程中", 0.2, 0.3, 0.7),
];

/// A single signal contributing to the AI signature score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiMarker {
    pub pattern: String,
    pub count: usize,
    pub density: f32,
    pub threshold: f32,
    pub expected_baseline: f32,
}

/// Aggregated AI writing signature report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiSignatureReport {
    /// Composite score: 0.0 = clearly human, 1.0 = strongly AI-generated.
    pub score: f32,
    /// Individual signals that contributed to the score.
    pub markers: Vec<AiMarker>,
    /// Top 3 contributing signal descriptions.
    pub top_signals: Vec<String>,
    /// Sentence length variability (standard deviation in chars).
    /// Low values indicate AI monotony. None if too few sentences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sentence_variability: Option<f32>,
    /// Count of zero-width tokenizer artifacts detected.
    pub zero_width_count: usize,
    /// Punctuation density matrix with per-type CV.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub punctuation_profile: Option<PunctuationProfile>,
}

// Terminal punctuation marks that delimit Chinese sentences.
const SENTENCE_TERMINATORS: &[char] = &['。', '！', '？', '!', '?'];

// Zero-width codepoints injected by LLM tokenizers (BPE/WordPiece).
const ZERO_WIDTH_CODEPOINTS: &[char] = &[
    '\u{200B}', // zero-width space
    '\u{200C}', // zero-width non-joiner
    '\u{200D}', // zero-width joiner
    '\u{FEFF}', // byte-order mark (mid-text = artifact)
    '\u{200E}', // left-to-right mark
    '\u{200F}', // right-to-left mark
];

/// Returns true if the character is a zero-width tokenizer artifact.
pub fn is_zero_width(ch: char) -> bool {
    ZERO_WIDTH_CODEPOINTS.contains(&ch)
}

// Punctuation types tracked in the density matrix.
const PUNCT_MARKS: &[(char, &str)] = &[
    ('，', "comma"),
    ('。', "period"),
    ('；', "semicolon"),
    ('、', "dunhao"),
];

/// Per-type punctuation statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PunctuationStat {
    pub count: usize,
    /// Density per 1000 characters.
    pub density: f32,
    /// Coefficient of variation of inter-punctuation distances.
    /// None if count < 10 (insufficient data for stable CV).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cv: Option<f32>,
}

/// Punctuation density matrix across major zh-TW mark types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PunctuationProfile {
    pub comma: PunctuationStat,
    pub period: PunctuationStat,
    pub semicolon: PunctuationStat,
    pub dunhao: PunctuationStat,
    pub dash: PunctuationStat,
}

/// Compute CV (coefficient of variation) from inter-mark distances.
/// Returns None if fewer than 10 occurrences (fewer than 9 distances).
fn compute_cv(distances: &[usize]) -> Option<f32> {
    if distances.len() < 9 {
        return None;
    }
    let n = distances.len() as f64;
    let mean = distances.iter().map(|&d| d as f64).sum::<f64>() / n;
    if mean < 1.0 {
        return None;
    }
    let variance = distances
        .iter()
        .map(|&d| (d as f64 - mean).powi(2))
        .sum::<f64>()
        / n;
    Some((variance.sqrt() / mean) as f32)
}

/// Compute punctuation density matrix, respecting exclusion zones.
fn compute_punctuation_profile(
    text: &str,
    text_k: f32,
    excluded: &[ByteRange],
) -> PunctuationProfile {
    // Collect positions using a visible-char index that only advances
    // outside exclusion zones, so excluded content (code blocks, URLs)
    // does not inflate inter-punctuation distances.
    let mut positions: [Vec<usize>; 4] = [vec![], vec![], vec![], vec![]];
    let mut dash_positions: Vec<usize> = Vec::new();

    let mut byte_offset = 0;
    let mut visible_idx: usize = 0;
    let chars: Vec<char> = text.chars().collect();
    for (char_idx, &ch) in chars.iter().enumerate() {
        let ch_len = ch.len_utf8();
        if !is_excluded(byte_offset, byte_offset + ch_len, excluded) {
            for (slot, &(mark, _)) in PUNCT_MARKS.iter().enumerate() {
                if ch == mark {
                    positions[slot].push(visible_idx);
                }
            }
            // Em-dash: two consecutive '—' chars.
            if ch == '—' && char_idx + 1 < chars.len() && chars[char_idx + 1] == '—' {
                dash_positions.push(visible_idx);
            }
            visible_idx += 1;
        }
        byte_offset += ch_len;
    }

    // Build stats for each type.
    let build_stat = |pos: &[usize]| -> PunctuationStat {
        let count = pos.len();
        let density = count as f32 / text_k;
        let distances: Vec<usize> = pos.windows(2).map(|w| w[1].saturating_sub(w[0])).collect();
        let cv = compute_cv(&distances);
        PunctuationStat { count, density, cv }
    };

    PunctuationProfile {
        comma: build_stat(&positions[0]),
        period: build_stat(&positions[1]),
        semicolon: build_stat(&positions[2]),
        dunhao: build_stat(&positions[3]),
        dash: build_stat(&dash_positions),
    }
}

/// Compute sentence length variability (standard deviation of char counts).
///
/// Splits on terminal punctuation, filters out fragments < 4 chars,
/// requires >= 10 sentences for statistical significance.
/// Characters whose byte offsets fall in `excluded` ranges are skipped.
fn compute_sentence_variability(text: &str, excluded: &[ByteRange]) -> Option<f32> {
    let mut lengths: Vec<usize> = Vec::new();
    let mut current = 0usize;
    let mut byte_offset = 0usize;
    let mut was_excluded = false;
    for ch in text.chars() {
        let ch_len = ch.len_utf8();
        let in_excluded = is_excluded(byte_offset, byte_offset + ch_len, excluded);
        byte_offset += ch_len;
        if in_excluded {
            // Treat exclusion boundaries as sentence breaks so adjacent
            // sentences are not fused when a code block sits between them.
            if !was_excluded && current >= 4 {
                lengths.push(current);
                current = 0;
            }
            was_excluded = true;
            continue;
        }
        was_excluded = false;
        if SENTENCE_TERMINATORS.contains(&ch) {
            if current >= 4 {
                lengths.push(current);
            }
            current = 0;
        } else {
            current += 1;
        }
    }
    if lengths.len() < 10 {
        return None;
    }
    // Accumulate in f64 to avoid catastrophic cancellation on large documents.
    let n = lengths.len() as f64;
    let mean = lengths.iter().map(|&l| l as f64).sum::<f64>() / n;
    let variance = lengths
        .iter()
        .map(|&l| (l as f64 - mean).powi(2))
        .sum::<f64>()
        / n;
    Some(variance.sqrt() as f32)
}

/// Count zero-width tokenizer artifact codepoints in text (excluding exclusion zones).
fn count_zero_width(text: &str, excluded: &[ByteRange]) -> usize {
    let mut count = 0;
    let mut byte_offset = 0;
    for ch in text.chars() {
        let ch_len = ch.len_utf8();
        if ZERO_WIDTH_CODEPOINTS.contains(&ch)
            && !is_excluded(byte_offset, byte_offset + ch_len, excluded)
        {
            count += 1;
        }
        byte_offset += ch_len;
    }
    count
}

/// Compute AI signature report from text and post-scan issues.
///
/// Combines six signal sources:
/// 1. Phrase density: count tracked phrases, compute density per 1000 chars.
/// 2. Structural patterns: count AiStyle issues from structural detectors.
/// 3. Per-occurrence: count non-structural/non-token AiStyle issues.
/// 4. Sentence length variability: low stddev = AI monotony.
/// 5. Zero-width tokenizer artifacts: BPE/WordPiece residue.
/// 6. Punctuation density matrix: aggregate CV of inter-punctuation distances.
///
/// Returns None for texts too short to analyze (< 500 chars).
pub fn compute_ai_score(
    text: &str,
    issues: &[Issue],
    excluded: &[ByteRange],
    threshold_multiplier: f32,
) -> Option<AiSignatureReport> {
    // Guard against zero/negative multiplier to prevent div-by-zero in thresholds.
    let threshold_multiplier = if threshold_multiplier <= 0.0 {
        1.0
    } else {
        threshold_multiplier
    };
    // Count only chars whose byte offsets fall outside excluded ranges.
    let char_count = {
        let mut count = 0usize;
        let mut byte_offset = 0usize;
        for ch in text.chars() {
            let ch_len = ch.len_utf8();
            if !is_excluded(byte_offset, byte_offset + ch_len, excluded) {
                count += 1;
            }
            byte_offset += ch_len;
        }
        count
    };
    if char_count < 500 {
        return None;
    }
    let text_k = char_count as f32 / 1000.0;

    let mut markers = Vec::new();
    let mut weighted_sum: f32 = 0.0;
    let mut total_weight: f32 = 0.0;

    // Signal 1: phrase density.  Apply threshold_multiplier so low/high
    // sensitivity affects the composite score, not just per-issue generation.
    for &(phrase, baseline, raw_threshold, weight) in DENSITY_SIGNALS {
        let threshold = raw_threshold * threshold_multiplier;
        let phrase_len = phrase.len();
        let mut count: usize = 0;
        let mut start = 0;
        while let Some(pos) = text[start..].find(phrase) {
            let abs = start + pos;
            start = abs + phrase_len;
            if !is_excluded(abs, abs + phrase_len, excluded) {
                count += 1;
            }
        }
        if count == 0 {
            continue;
        }
        let density = count as f32 / text_k;
        markers.push(AiMarker {
            pattern: phrase.to_string(),
            count,
            density,
            threshold,
            expected_baseline: baseline,
        });
        if density > threshold {
            // Normalized contribution: how far above threshold, capped at 1.0.
            let excess = ((density - threshold) / threshold).min(2.0);
            weighted_sum += excess * weight;
        }
        total_weight += weight;
    }

    // Signal 2: structural pattern count from existing issues.
    let structural_count = issues
        .iter()
        .filter(|i| {
            i.rule_type == IssueType::AiStyle
                && i.context
                    .as_ref()
                    .is_some_and(|c| c.starts_with("AI structural:"))
        })
        .count();
    // Each structural signal contributes 0.1 to score (max 0.4 for 4 signals).
    // Rebalanced from 0.15/0.75 per 40.11: structural should not dominate phrase density.
    let structural_contribution = (structural_count as f32 * 0.1).min(0.4);

    // Signal 3: non-structural, non-zero-width AiStyle issue density.
    // Excludes issues already counted by signals 1 (density phrases),
    // 2 (structural), and 5 (zero-width) to avoid double-counting.
    let ai_issue_count = issues
        .iter()
        .filter(|i| {
            i.rule_type == IssueType::AiStyle
                && !i
                    .context
                    .as_ref()
                    .is_some_and(|c| c.starts_with("AI structural:") || c.starts_with("AI token:"))
                && !DENSITY_SIGNALS
                    .iter()
                    .any(|&(phrase, _, _, _)| i.found == phrase)
        })
        .count();
    let ai_issue_density = ai_issue_count as f32 / text_k;
    // High density of AI issues is itself a signal.  Threshold: >2 per 1000 chars.
    let issue_density_contribution = if ai_issue_density > 2.0 {
        ((ai_issue_density - 2.0) / 5.0).min(0.3)
    } else {
        0.0
    };

    // Signal 4: sentence length variability (low stddev = AI monotony).
    let sentence_variability = compute_sentence_variability(text, excluded);
    let var_threshold = 5.0 * threshold_multiplier;
    let variability_contribution = match sentence_variability {
        Some(sigma) if sigma < var_threshold => {
            // sigma below threshold is suspiciously uniform; max contribution 0.15.
            ((var_threshold - sigma) / var_threshold * 0.15).min(0.15)
        }
        _ => 0.0,
    };

    // Signal 5: zero-width tokenizer artifacts.
    let zero_width_count = count_zero_width(text, excluded);
    let zw_scale = 3.0 * threshold_multiplier;
    let zero_width_contribution = if zero_width_count > 0 {
        // Any presence is suspicious; 3+ is strong signal. Max 0.2.
        ((zero_width_count as f32) / zw_scale * 0.2).min(0.2)
    } else {
        0.0
    };

    // Signal 6: punctuation density matrix — aggregate CV.
    let punctuation_profile = compute_punctuation_profile(text, text_k, excluded);
    let punct_contribution = {
        // Aggregate CV across types with sufficient samples (N >= 10),
        // weighted by occurrence count.
        let stats = [
            &punctuation_profile.comma,
            &punctuation_profile.period,
            &punctuation_profile.semicolon,
            &punctuation_profile.dunhao,
            &punctuation_profile.dash,
        ];
        let mut weighted_cv_sum = 0.0f64;
        let mut total_count = 0usize;
        for stat in &stats {
            if let Some(cv) = stat.cv {
                weighted_cv_sum += cv as f64 * stat.count as f64;
                total_count += stat.count;
            }
        }
        if total_count > 0 {
            let aggregate_cv = (weighted_cv_sum / total_count as f64) as f32;
            // Low CV (< threshold) = uniform rhythm = AI signal.  Max 0.1.
            let cv_threshold = 0.5 * threshold_multiplier;
            ((cv_threshold - aggregate_cv).max(0.0) / cv_threshold * 0.1).min(0.1)
        } else {
            0.0
        }
    };

    // Composite score: combine all six signals (rebalanced per 40.11).
    // phrase ≤0.7, structural ≤0.4, issue ≤0.3, variability ≤0.15,
    // zero-width ≤0.2, punctuation ≤0.1.  Max ~1.85 before clamp.
    // No single signal exceeds 0.7; ≥0.8 requires ≥2 dimensions.
    let phrase_score = if total_weight > 0.0 {
        (weighted_sum / total_weight).min(1.0) * 0.7
    } else {
        0.0
    };
    let raw_score = phrase_score
        + structural_contribution
        + issue_density_contribution
        + variability_contribution
        + zero_width_contribution
        + punct_contribution;
    let score = raw_score.min(1.0);

    // Build top signals list (sorted by density excess ratio).
    markers.sort_by(|a, b| {
        let a_ratio = a.density / a.threshold;
        let b_ratio = b.density / b.threshold;
        b_ratio
            .partial_cmp(&a_ratio)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut top_signals: Vec<String> = markers
        .iter()
        .filter(|m| m.density > m.threshold)
        .take(3)
        .map(|m| {
            format!(
                "\u{300C}{}\u{300D} {:.1}次/千字 (閾值 {})",
                m.pattern, m.density, m.threshold
            )
        })
        .collect();
    if structural_count > 0 {
        top_signals.push(format!("{structural_count} 個結構性 AI 特徵"));
    }
    if let Some(sigma) = sentence_variability {
        if sigma < var_threshold {
            top_signals.push(format!("句長變異低 σ={sigma:.1}（疑似 AI 均質化）"));
        }
    }
    if zero_width_count > 0 {
        top_signals.push(format!("{zero_width_count} 個零寬字元（疑似分詞器殘留）"));
    }
    if punct_contribution > 0.0 {
        top_signals.push("標點節奏過於均勻（疑似 AI 生成）".to_string());
    }
    top_signals.truncate(3);

    let punctuation_profile =
        if punctuation_profile.comma.cv.is_some() || punctuation_profile.period.cv.is_some() {
            Some(punctuation_profile)
        } else {
            None
        };

    Some(AiSignatureReport {
        score,
        markers,
        top_signals,
        sentence_variability,
        zero_width_count,
        punctuation_profile,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_returns_none() {
        let result = compute_ai_score("短文", &[], &[], 1.0);
        assert!(result.is_none());
    }

    #[test]
    fn clean_text_low_score() {
        let text = "台灣的半導體產業在全球市場中佔有重要地位。".repeat(30);
        let result = compute_ai_score(&text, &[], &[], 1.0);
        let report = result.unwrap();
        assert!(
            report.score <= 0.3,
            "clean text should score low: {:.2}",
            report.score
        );
    }

    #[test]
    fn ai_heavy_text_high_score() {
        // Build text loaded with AI patterns.
        let filler = "這是正常的技術段落。";
        let mut text = String::new();
        for i in 0..80 {
            match i % 8 {
                0 => text.push_str("更重要的是，這個技術非常關鍵。"),
                1 => text.push_str("值得注意的是，我們發現了新的問題。"),
                2 => text.push_str("這意味著我們需要重新評估方案。"),
                3 => text.push_str("不容忽視的影響深遠。"),
                4 => text.push_str("深刻影響了整個產業的發展。"),
                _ => text.push_str(filler),
            }
        }
        // Add some structural pattern issues.
        let structural_issues: Vec<Issue> = (0..3)
            .map(|i| {
                Issue::new(
                    i,
                    1,
                    "",
                    vec![],
                    IssueType::AiStyle,
                    crate::rules::ruleset::Severity::Info,
                )
                .with_context("AI structural: test pattern")
            })
            .collect();
        let result = compute_ai_score(&text, &structural_issues, &[], 1.0);
        let report = result.unwrap();
        assert!(
            report.score >= 0.5,
            "AI-heavy text should score high: {:.2}",
            report.score
        );
        assert!(!report.markers.is_empty());
        assert!(!report.top_signals.is_empty());
    }

    #[test]
    fn sentence_variability_uniform_low() {
        // All sentences nearly identical length -> low sigma -> contributes to score.
        let sentence = "這是一段長度相同的句子內容";
        let mut text = String::new();
        for _ in 0..60 {
            text.push_str(sentence);
            text.push('。');
        }
        let result = compute_ai_score(&text, &[], &[], 1.0);
        let report = result.unwrap();
        assert!(
            report.sentence_variability.is_some(),
            "should compute variability for 60 sentences"
        );
        let sigma = report.sentence_variability.unwrap();
        assert!(
            sigma < 2.0,
            "uniform sentences should have low sigma: {sigma:.1}"
        );
    }

    #[test]
    fn sentence_variability_varied_high() {
        // Mix of short (>=4 chars) and very long sentences -> high sigma.
        let mut text = String::new();
        for i in 0..30 {
            if i % 2 == 0 {
                text.push_str("這是短句。");
            } else {
                text.push_str(
                    &"這是一段非常非常非常非常非常冗長的句子用來增加長度變異性".repeat(3),
                );
                text.push('。');
            }
        }
        let result = compute_ai_score(&text, &[], &[], 1.0);
        let report = result.unwrap();
        let sigma = report
            .sentence_variability
            .expect("should compute variability for varied sentences");
        assert!(
            sigma > 10.0,
            "varied sentences should have high sigma: {sigma:.1}"
        );
    }

    #[test]
    fn zero_width_detection() {
        let mut text = "台灣的半導體產業在全球市場中佔有重要地位。".repeat(30);
        // Inject zero-width spaces.
        text.push('\u{200B}');
        text.push_str("更多文字");
        text.push('\u{FEFF}');
        text.push_str("結尾。");
        let result = compute_ai_score(&text, &[], &[], 1.0);
        let report = result.unwrap();
        assert_eq!(
            report.zero_width_count, 2,
            "should detect 2 zero-width chars"
        );
    }

    #[test]
    fn zero_width_excluded() {
        let mut text = "台灣的半導體產業在全球市場中佔有重要地位。".repeat(30);
        let zw_offset = text.len();
        text.push('\u{200B}');
        let excluded = vec![ByteRange {
            start: zw_offset,
            end: zw_offset + 3,
        }];
        let result = compute_ai_score(&text, &[], &excluded, 1.0);
        let report = result.unwrap();
        assert_eq!(
            report.zero_width_count, 0,
            "excluded zero-width should not count"
        );
    }

    #[test]
    fn punctuation_profile_uniform_rhythm() {
        // AI-like text: commas at perfectly regular intervals.
        let clause = "這是一個測試，";
        let mut text = String::new();
        for _ in 0..80 {
            text.push_str(clause);
        }
        // Add enough periods for the profile to be computed.
        for _ in 0..15 {
            text.push_str("這是句子結尾。");
        }
        let result = compute_ai_score(&text, &[], &[], 1.0);
        let report = result.unwrap();
        if let Some(ref profile) = report.punctuation_profile {
            assert!(
                profile.comma.count >= 10,
                "should have enough commas: {}",
                profile.comma.count
            );
            if let Some(cv) = profile.comma.cv {
                assert!(
                    cv < 0.3,
                    "uniform comma spacing should have low CV: {cv:.2}"
                );
            }
        }
    }

    #[test]
    fn punctuation_profile_varied_rhythm() {
        // Human-like text: wildly varying clause lengths.
        let mut text = String::new();
        for i in 0..40 {
            if i % 3 == 0 {
                text.push_str("短，");
            } else if i % 3 == 1 {
                text.push_str("這是一段比較長的句子用來增加變異性，");
            } else {
                text.push_str(
                    "這是一段非常非常非常非常冗長的句子，目的是讓逗號間距的變異係數升高，",
                );
            }
        }
        for _ in 0..15 {
            text.push_str("結尾句子。");
        }
        let result = compute_ai_score(&text, &[], &[], 1.0);
        let report = result.unwrap();
        if let Some(ref profile) = report.punctuation_profile {
            if let Some(cv) = profile.comma.cv {
                assert!(
                    cv >= 0.4,
                    "varied comma spacing should have moderate-to-high CV: {cv:.2}"
                );
            }
        }
    }

    #[test]
    fn punctuation_profile_sparse_no_cv() {
        // Text with very few commas — CV should be None.
        let text = "台灣的半導體產業在全球市場中佔有重要地位。".repeat(30);
        let result = compute_ai_score(&text, &[], &[], 1.0);
        let report = result.unwrap();
        if let Some(ref profile) = report.punctuation_profile {
            assert!(
                profile.comma.cv.is_none(),
                "sparse commas should yield no CV"
            );
        }
    }

    #[test]
    fn excluded_ranges_respected() {
        let mut text = String::new();
        for _ in 0..60 {
            text.push_str("更重要的是，這很重要。");
        }
        // Exclude entire text.
        let excluded = vec![ByteRange {
            start: 0,
            end: text.len(),
        }];
        let result = compute_ai_score(&text, &[], &excluded, 1.0);
        assert!(
            result.is_none(),
            "fully excluded text should return None (below char_count threshold)"
        );
    }
}
