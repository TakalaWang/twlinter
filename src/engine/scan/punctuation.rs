// Punctuation scanning: half-width to full-width detection, dunhao
// (enumeration comma), and range indicator normalization.

use crate::engine::excluded::{is_excluded, ByteRange};
use crate::rules::ruleset::{Issue, ProfileConfig, Severity};

use super::{
    adjacent_cjk, adjacent_cjk_inner, has_paragraph_break, immediate_cjk, punct_issue, Scanner,
};

impl Scanner {
    /// Punctuation scan: detect half-width punctuation that should be full-width
    /// in a CJK context.
    ///
    /// Handles: , . ! ? ; ( ) : (2.1 + 2.2).
    /// Colon enforcement is profile-dependent: relaxed allows half-width :.
    pub(crate) fn scan_punctuation(
        &self,
        text: &str,
        excluded: &[ByteRange],
        issues: &mut Vec<Issue>,
        cfg: &ProfileConfig,
    ) {
        let bytes = text.as_bytes();
        let len = bytes.len();
        let mut ascii_quote_prev_end = 0usize;
        let mut ascii_quote_pos_in_para = 0usize;

        for (i, &b) in bytes.iter().enumerate() {
            match b {
                b',' | b'.' | b'!' | b'?' | b';' | b'(' | b')' | b':' | b'"' => {}
                _ => continue,
            }

            if is_excluded(i, i + 1, excluded) {
                continue;
            }

            match b {
                b',' => {
                    // Guard: digit on both sides → thousands separator (e.g. 1,000).
                    let digit_before = i > 0 && bytes[i - 1].is_ascii_digit();
                    let digit_after = i + 1 < len && bytes[i + 1].is_ascii_digit();
                    if digit_before && digit_after {
                        continue;
                    }
                    if !adjacent_cjk(text, i, true) && !adjacent_cjk(text, i + 1, false) {
                        continue;
                    }
                    issues.push(punct_issue(
                        i,
                        ",",
                        "\u{FF0C}",
                        "繁體中文應使用全形逗號「，」而非半形逗號「,」",
                    ));
                }
                b'.' => {
                    // Guard: adjacent period → ellipsis (.. or ...).
                    let period_before = i > 0 && bytes[i - 1] == b'.';
                    let period_after = i + 1 < len && bytes[i + 1] == b'.';
                    if period_before || period_after {
                        continue;
                    }
                    // Guard: followed by ASCII alphanumeric → decimal / extension.
                    if i + 1 < len && bytes[i + 1].is_ascii_alphanumeric() {
                        continue;
                    }
                    if !adjacent_cjk(text, i, true) {
                        continue;
                    }
                    issues.push(punct_issue(
                        i,
                        ".",
                        "\u{3002}",
                        "繁體中文應使用全形句號「。」而非半形句號「.」",
                    ));
                }
                b'!' | b'?' | b';' => {
                    // Guard: Markdown image syntax ![alt](url) — ! followed by [
                    // is never a prose exclamation mark.
                    if b == b'!' && i + 1 < len && bytes[i + 1] == b'[' {
                        continue;
                    }
                    if !adjacent_cjk(text, i, true) && !adjacent_cjk(text, i + 1, false) {
                        continue;
                    }
                    let (found, suggestion, context) = match b {
                        b'!' => (
                            "!",
                            "\u{FF01}",
                            "繁體中文應使用全形驚嘆號「！」而非半形「!」",
                        ),
                        b'?' => ("?", "\u{FF1F}", "繁體中文應使用全形問號「？」而非半形「?」"),
                        _ => (";", "\u{FF1B}", "繁體中文應使用全形分號「；」而非半形「;」"),
                    };
                    issues.push(punct_issue(i, found, suggestion, context));
                }
                b'(' | b')' => {
                    // Require CJK immediately adjacent (no whitespace skip) on both
                    // sides to avoid flagging functional ASCII parens: method calls
                    // like foo(), markdown links [text](url), spaced 中文 (note).
                    if !immediate_cjk(text, i, true) || !immediate_cjk(text, i + 1, false) {
                        continue;
                    }
                    let (found, suggestion, context) = if b == b'(' {
                        (
                            "(",
                            "\u{FF08}",
                            "繁體中文應使用全形左括號「（」而非半形「(」",
                        )
                    } else {
                        (
                            ")",
                            "\u{FF09}",
                            "繁體中文應使用全形右括號「）」而非半形「)」",
                        )
                    };
                    issues.push(punct_issue(i, found, suggestion, context));
                }
                b':' => {
                    // Colon enforcement controlled by profile config.
                    if !cfg.colon_enforcement {
                        continue;
                    }
                    // Guard: digit on both sides → time format (e.g. 12:30).
                    let digit_before = i > 0 && bytes[i - 1].is_ascii_digit();
                    let digit_after = i + 1 < len && bytes[i + 1].is_ascii_digit();
                    if digit_before && digit_after {
                        continue;
                    }
                    // Guard: followed by // → protocol (e.g. http://).
                    if i + 2 < len && bytes[i + 1] == b'/' && bytes[i + 2] == b'/' {
                        continue;
                    }
                    // Guard: ]: → Markdown reference/footnote definition ([^id]: text, [id]: url).
                    if i > 0 && bytes[i - 1] == b']' {
                        continue;
                    }
                    // Guard: definition-list colon — `: ` at the start of a line
                    // (possibly indented) is Markdown structural markup.
                    // Pattern: (BOF or \n)(spaces/tabs)*`: `.
                    if i + 1 < len && bytes[i + 1] == b' ' {
                        let line_start = if i == 0 {
                            true
                        } else {
                            // Walk backwards over spaces/tabs to find \n or BOF.
                            let mut j = i - 1;
                            loop {
                                if bytes[j] == b'\n' {
                                    break true;
                                }
                                if bytes[j] != b' ' && bytes[j] != b'\t' {
                                    break false;
                                }
                                if j == 0 {
                                    break true; // BOF after only whitespace
                                }
                                j -= 1;
                            }
                        };
                        if line_start {
                            continue;
                        }
                    }
                    if !adjacent_cjk(text, i, true) && !adjacent_cjk(text, i + 1, false) {
                        continue;
                    }
                    issues.push(punct_issue(
                        i,
                        ":",
                        "\u{FF1A}",
                        "繁體中文應使用全形冒號「：」而非半形「:」",
                    ));
                }
                b'"' => {
                    if i > ascii_quote_prev_end
                        && has_paragraph_break(text, ascii_quote_prev_end, i)
                    {
                        ascii_quote_pos_in_para = 0;
                    }

                    let left_cjk = adjacent_cjk_inner(text, i, true, 3);
                    let right_cjk = adjacent_cjk_inner(text, i + 1, false, 3);
                    let is_closing = !ascii_quote_pos_in_para.is_multiple_of(2);

                    if !left_cjk && !right_cjk && !is_closing {
                        continue;
                    }

                    let suggestion = if ascii_quote_pos_in_para.is_multiple_of(2) {
                        "\u{300c}" // 「
                    } else {
                        "\u{300d}" // 」
                    };
                    issues.push(punct_issue(
                        i,
                        "\"",
                        suggestion,
                        "繁體中文應使用「」引號而非半形雙引號「\"」",
                    ));
                    ascii_quote_prev_end = i + 1;
                    ascii_quote_pos_in_para += 1;
                }
                _ => unreachable!(),
            }
        }
    }

    /// Enumeration comma (dunhao) detection.
    ///
    /// Scans for sequences of short items separated by full-width ， that
    /// likely represent coordinate lists and should use 、 instead.
    /// Severity: Info (advisory -- the heuristic false-positives on short clauses).
    pub(crate) fn scan_dunhao(&self, text: &str, excluded: &[ByteRange], issues: &mut Vec<Issue>) {
        let comma = "\u{FF0C}"; // ，
        let comma_len = comma.len(); // 3 bytes
        let max_item_chars = 4;

        // Collect non-excluded full-width comma positions.
        let mut positions: Vec<usize> = Vec::new();
        let mut start = 0;
        while let Some(rel) = text[start..].find(comma) {
            let abs = start + rel;
            if !is_excluded(abs, abs + comma_len, excluded) {
                positions.push(abs);
            }
            start = abs + comma_len;
        }

        if positions.len() < 2 {
            return;
        }

        // is_short[j]: segment between positions[j] and positions[j+1] is 1-4 chars.
        let is_short: Vec<bool> = (0..positions.len() - 1)
            .map(|j| {
                let seg = text[positions[j] + comma_len..positions[j + 1]].trim();
                let count = seg.chars().count();
                count > 0 && count <= max_item_chars
            })
            .collect();

        // Find runs of consecutive short segments. A run of length N means
        // N+1 commas bounding N+2 items. Require N >= 2.
        let mut i = 0;
        while i < is_short.len() {
            if !is_short[i] {
                i += 1;
                continue;
            }
            let run_start = i;
            while i < is_short.len() && is_short[i] {
                i += 1;
            }
            let run_len = i - run_start;
            if run_len < 2 {
                continue;
            }
            for &pos in &positions[run_start..=i.min(positions.len() - 1)] {
                issues.push(super::punct_issue_sev(
                    pos,
                    "\u{FF0C}",
                    "\u{3001}",
                    "列舉項目建議使用頓號「、」而非逗號「，」",
                    Severity::Info,
                ));
            }
        }
    }

    /// CN curly quotation mark detection.
    ///
    /// Scans for CN-style curly double quotes \u{201c}/\u{201d} and single
    /// quotes \u{2018}/\u{2019}.  These are multi-byte UTF-8 characters that
    /// the byte-level ASCII scan in scan_punctuation() cannot detect.
    ///
    /// Requires CJK adjacency on at least one side to avoid false positives on
    /// English typographic smart quotes and apostrophes (e.g., "Hello" or it's).
    /// \u{2019} is the standard typographic apostrophe in English — without this
    /// guard, words like "don't" would be destroyed.
    ///
    /// Double quotes are emitted as issues; `fix_quote_pairing()` in quotes.rs
    /// then reassigns their suggestions with depth-based nesting (「」/『』).
    /// Single quotes map directly to 『/』 (secondary TW bracket quotes).
    pub(crate) fn scan_cn_curly_quotes(
        &self,
        text: &str,
        excluded: &[ByteRange],
        issues: &mut Vec<Issue>,
    ) {
        for (byte_offset, ch) in text.char_indices() {
            let ch_len = ch.len_utf8();
            match ch {
                '\u{201c}' | '\u{201d}' => {
                    if is_excluded(byte_offset, byte_offset + ch_len, excluded) {
                        continue;
                    }
                    // Require CJK context on at least one side to avoid flagging
                    // English smart quotes (e.g., "Hello," she said.).
                    let left_cjk = adjacent_cjk_inner(text, byte_offset, true, 3);
                    let right_cjk = adjacent_cjk_inner(text, byte_offset + ch_len, false, 3);
                    if !left_cjk && !right_cjk {
                        continue;
                    }
                    // Placeholder suggestion; fix_quote_pairing() overwrites with
                    // depth-aware nesting.
                    let suggestion = if ch == '\u{201c}' {
                        "\u{300c}" // 「
                    } else {
                        "\u{300d}" // 」
                    };
                    issues.push(punct_issue(
                        byte_offset,
                        &text[byte_offset..byte_offset + ch_len],
                        suggestion,
                        "繁體中文應使用「」引號而非中國大陸式「\u{201c}\u{201d}」",
                    ));
                }
                '\u{2018}' | '\u{2019}' => {
                    if is_excluded(byte_offset, byte_offset + ch_len, excluded) {
                        continue;
                    }
                    // Guard: ASCII letter immediately adjacent means English
                    // apostrophe/contraction (it's, don't, 's, 'll), not a CN
                    // quote.  This catches "中文's" and "中文 'twas" that would
                    // otherwise false-positive due to nearby CJK context.
                    let ascii_before =
                        byte_offset > 0 && text.as_bytes()[byte_offset - 1].is_ascii_alphabetic();
                    let ascii_after = byte_offset + ch_len < text.len()
                        && text.as_bytes()[byte_offset + ch_len].is_ascii_alphabetic();
                    if ascii_before || ascii_after {
                        continue;
                    }
                    // Require CJK context on at least one side to avoid flagging
                    // English typographic apostrophes in pure-English text.
                    let left_cjk = adjacent_cjk_inner(text, byte_offset, true, 3);
                    let right_cjk = adjacent_cjk_inner(text, byte_offset + ch_len, false, 3);
                    if !left_cjk && !right_cjk {
                        continue;
                    }
                    let suggestion = if ch == '\u{2018}' {
                        "\u{300e}" // 『
                    } else {
                        "\u{300f}" // 』
                    };
                    issues.push(punct_issue(
                        byte_offset,
                        &text[byte_offset..byte_offset + ch_len],
                        suggestion,
                        "繁體中文應使用『』引號而非中國大陸式「\u{2018}\u{2019}」",
                    ));
                }
                _ => {}
            }
        }
    }

    /// Range indicator normalization.
    ///
    /// Detects ~ or - used as range indicators in CJK context and suggests
    /// the profile-appropriate full-width form: ～ (wave dash) for prose,
    /// – (en dash) for technical/UI contexts.
    pub(crate) fn scan_range_indicators(
        &self,
        text: &str,
        excluded: &[ByteRange],
        issues: &mut Vec<Issue>,
        cfg: &ProfileConfig,
    ) {
        let bytes = text.as_bytes();
        let len = bytes.len();

        let suggestion = if cfg.range_en_dash {
            "\u{2013}" // – (en dash)
        } else {
            "\u{FF5E}" // ～ (wave dash)
        };

        for (i, &b) in bytes.iter().enumerate() {
            if b != b'~' && b != b'-' {
                continue;
            }
            if is_excluded(i, i + 1, excluded) {
                continue;
            }

            if b == b'~' {
                // Tilde as range indicator: digit~digit or CJK~CJK.
                let left_digit = i > 0 && bytes[i - 1].is_ascii_digit();
                let right_digit = i + 1 < len && bytes[i + 1].is_ascii_digit();
                let left_cjk = adjacent_cjk(text, i, true);
                let right_cjk = adjacent_cjk(text, i + 1, false);

                if !(left_digit || left_cjk) || !(right_digit || right_cjk) {
                    continue;
                }
                // Require at least one CJK side to avoid flagging in pure ASCII.
                if !left_cjk && !right_cjk {
                    continue;
                }
                // Guard: unary approximation ~N.
                if right_digit && !left_digit {
                    continue;
                }
            } else {
                // Hyphen as range indicator. Very conservative to avoid
                // false positives on markdown, CLI flags, minus signs.
                // Skip consecutive dashes.
                if (i > 0 && bytes[i - 1] == b'-') || (i + 1 < len && bytes[i + 1] == b'-') {
                    continue;
                }
                // Guard: Markdown list bullet — skip if - is at line start.
                let is_line_start = {
                    let mut j = i;
                    while j > 0 && (bytes[j - 1] == b' ' || bytes[j - 1] == b'\t') {
                        j -= 1;
                    }
                    j == 0 || bytes[j - 1] == b'\n' || bytes[j - 1] == b'\r'
                };
                if is_line_start {
                    continue;
                }
                // Only flag when both adjacent non-whitespace chars are CJK.
                if !adjacent_cjk(text, i, true) || !adjacent_cjk(text, i + 1, false) {
                    continue;
                }
            }

            let found = if b == b'~' { "~" } else { "-" };
            issues.push(super::punct_issue_sev(
                i,
                found,
                suggestion,
                "範圍表示建議使用全形波浪號「～」或半形連接號「–」",
                Severity::Info,
            ));
        }
    }
}
