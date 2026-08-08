// Inline suppression mechanism for zhtw-mcp linting.
//
// Users can suppress linting for specific lines or blocks using comments:
//   Markdown: <!-- zhtw:ignore-next-line --> or <!-- zhtw:disable-next-line -->
//   Markdown: <!-- zhtw:ignore-block --> ... <!-- zhtw:end-ignore -->
//   Markdown: <!-- zhtw:disable-block --> ... <!-- zhtw:end-disable -->
//   Code:     // zhtw:ignore or // zhtw:disable (line-level)
//
// The disable variants are aliases for the ignore variants, provided
// for users who prefer the disable/enable terminology common in other linters
// (e.g. eslint-disable-next-line).  Both spellings are equally supported.
//
// Suppressed ranges are merged into excluded ranges before scanning.

use super::excluded::ByteRange;

/// Returns true if the trimmed line contains a next-line suppression marker.
/// Accepts both the ignore and disable spellings.
#[inline]
fn is_next_line_suppression(trimmed: &str) -> bool {
    trimmed.contains("<!-- zhtw:ignore-next-line -->")
        || trimmed.contains("<!-- zhtw:disable-next-line -->")
}

/// Returns true if the trimmed line contains a block-start suppression marker.
#[inline]
fn is_block_start(trimmed: &str) -> bool {
    trimmed.contains("<!-- zhtw:ignore-block -->")
        || trimmed.contains("<!-- zhtw:disable-block -->")
}

/// Returns true if the trimmed line contains a block-end suppression marker.
#[inline]
fn is_block_end(trimmed: &str) -> bool {
    trimmed.contains("<!-- zhtw:end-ignore -->") || trimmed.contains("<!-- zhtw:end-disable -->")
}

/// Scan text for suppression markers and return byte ranges to exclude.
///
/// Supported markers:
/// - <!-- zhtw:ignore-next-line --> / <!-- zhtw:disable-next-line -->:
///   suppresses the entire next line
/// - <!-- zhtw:ignore-block --> … <!-- zhtw:end-ignore -->:
///   suppresses a multi-line block
/// - <!-- zhtw:disable-block --> … <!-- zhtw:end-disable -->:
///   alias for the ignore-block pair
/// - // zhtw:ignore / // zhtw:disable: suppresses the current line (from start of line)
pub fn build_suppression_ranges(text: &str) -> Vec<ByteRange> {
    let mut ranges = Vec::new();

    // Track block suppression state.
    let mut in_block = false;
    let mut block_start = 0usize;

    for (line_start, line) in LineIter::new(text) {
        let trimmed = line.trim();
        let line_end = line_start + line.len();

        // Check for block end first (so we can close an open block).
        if in_block {
            if is_block_end(trimmed) {
                // Suppress from block_start to end of this line (inclusive).
                ranges.push(ByteRange {
                    start: block_start,
                    end: line_end,
                });
                in_block = false;
                continue;
            }
            // Still in block, will be handled when block ends.
            continue;
        }

        // Check for block start.
        if is_block_start(trimmed) {
            in_block = true;
            block_start = line_start;
            continue;
        }

        // Check for next-line suppression.
        if is_next_line_suppression(trimmed) {
            // Suppress the line that follows this one.
            // Find the start of the next line.
            if line_end < text.len() {
                let next_line_start = line_end;
                // Find end of next line.
                let next_line_end = text[next_line_start..]
                    .find('\n')
                    .map(|pos| next_line_start + pos + 1)
                    .unwrap_or(text.len());
                ranges.push(ByteRange {
                    start: next_line_start,
                    end: next_line_end,
                });
            }
            continue;
        }

        // Check for inline code suppression: // zhtw:ignore or // zhtw:disable
        if trimmed.contains("// zhtw:ignore") || trimmed.contains("// zhtw:disable") {
            ranges.push(ByteRange {
                start: line_start,
                end: line_end,
            });
        }
    }

    // Unclosed block: suppress from block_start to end of text.
    if in_block {
        ranges.push(ByteRange {
            start: block_start,
            end: text.len(),
        });
    }

    ranges
}

/// Iterator over lines in a string, yielding (byte_start, line_text) pairs.
/// Line text includes the trailing newline if present.
struct LineIter<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> LineIter<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, pos: 0 }
    }
}

impl<'a> Iterator for LineIter<'a> {
    type Item = (usize, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.text.len() {
            return None;
        }
        let start = self.pos;
        let rest = &self.text[start..];
        let line_len = rest.find('\n').map(|i| i + 1).unwrap_or(rest.len());
        self.pos = start + line_len;
        Some((start, &self.text[start..self.pos]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_next_line_markdown() {
        let text = "第一行\n<!-- zhtw:ignore-next-line -->\n這行被忽略\n第四行\n";
        let ranges = build_suppression_ranges(text);
        assert_eq!(ranges.len(), 1);
        // The suppressed line is "這行被忽略\n"
        let suppressed = &text[ranges[0].start..ranges[0].end];
        assert!(suppressed.contains("這行被忽略"));
    }

    #[test]
    fn ignore_next_line_at_end() {
        // Suppress marker on last line with no following line.
        let text = "第一行\n<!-- zhtw:ignore-next-line -->";
        let ranges = build_suppression_ranges(text);
        assert!(ranges.is_empty()); // No next line to suppress.
    }

    #[test]
    fn ignore_block_markdown() {
        let text =
            "開始\n<!-- zhtw:ignore-block -->\n被忽略1\n被忽略2\n<!-- zhtw:end-ignore -->\n結束\n";
        let ranges = build_suppression_ranges(text);
        assert_eq!(ranges.len(), 1);
        let suppressed = &text[ranges[0].start..ranges[0].end];
        assert!(suppressed.contains("被忽略1"));
        assert!(suppressed.contains("被忽略2"));
        assert!(!suppressed.contains("結束"));
    }

    #[test]
    fn ignore_block_unclosed() {
        let text = "開始\n<!-- zhtw:ignore-block -->\n被忽略到結尾";
        let ranges = build_suppression_ranges(text);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].end, text.len());
    }

    #[test]
    fn inline_code_suppression() {
        let text = "正常行\n被忽略 // zhtw:ignore\n正常行\n";
        let ranges = build_suppression_ranges(text);
        assert_eq!(ranges.len(), 1);
        let suppressed = &text[ranges[0].start..ranges[0].end];
        assert!(suppressed.contains("被忽略"));
    }

    #[test]
    fn no_suppression_markers() {
        let text = "這是正常文字\n沒有任何忽略標記\n";
        let ranges = build_suppression_ranges(text);
        assert!(ranges.is_empty());
    }

    #[test]
    fn multiple_suppressions() {
        let text = "行1\n<!-- zhtw:ignore-next-line -->\n忽略2\n行3 // zhtw:ignore\n行4\n";
        let ranges = build_suppression_ranges(text);
        assert_eq!(ranges.len(), 2);
    }

    #[test]
    fn empty_text() {
        let ranges = build_suppression_ranges("");
        assert!(ranges.is_empty());
    }

    // disable alias tests

    #[test]
    fn disable_next_line_alias() {
        let text = "第一行\n<!-- zhtw:disable-next-line -->\n這行被忽略\n第四行\n";
        let ranges = build_suppression_ranges(text);
        assert_eq!(ranges.len(), 1);
        let suppressed = &text[ranges[0].start..ranges[0].end];
        assert!(suppressed.contains("這行被忽略"));
    }

    #[test]
    fn disable_block_alias() {
        let text =
            "開始\n<!-- zhtw:disable-block -->\n被忽略1\n被忽略2\n<!-- zhtw:end-disable -->\n結束\n";
        let ranges = build_suppression_ranges(text);
        assert_eq!(ranges.len(), 1);
        let suppressed = &text[ranges[0].start..ranges[0].end];
        assert!(suppressed.contains("被忽略1"));
        assert!(suppressed.contains("被忽略2"));
        assert!(!suppressed.contains("結束"));
    }

    #[test]
    fn end_disable_closes_ignore_block() {
        // end-disable should close ignore-block and vice-versa.
        let text = "開始\n<!-- zhtw:ignore-block -->\n被忽略1\n<!-- zhtw:end-disable -->\n結束\n";
        let ranges = build_suppression_ranges(text);
        assert_eq!(ranges.len(), 1);
        let suppressed = &text[ranges[0].start..ranges[0].end];
        assert!(suppressed.contains("被忽略1"));
        assert!(!suppressed.contains("結束"));
    }

    #[test]
    fn inline_code_disable_alias() {
        // // zhtw:disable should work identically to // zhtw:ignore for inline code.
        let text = "正常行\n被忽略 // zhtw:disable\n正常行\n";
        let ranges = build_suppression_ranges(text);
        assert_eq!(ranges.len(), 1);
        let suppressed = &text[ranges[0].start..ranges[0].end];
        assert!(suppressed.contains("被忽略"));
    }

    #[test]
    fn end_ignore_closes_disable_block() {
        // Reciprocal of end_disable_closes_ignore_block:
        // end-ignore should close a block opened by disable-block.
        let text = "開始\n<!-- zhtw:disable-block -->\n被忽略1\n<!-- zhtw:end-ignore -->\n結束\n";
        let ranges = build_suppression_ranges(text);
        assert_eq!(ranges.len(), 1);
        let suppressed = &text[ranges[0].start..ranges[0].end];
        assert!(suppressed.contains("被忽略1"));
        assert!(!suppressed.contains("結束"));
    }
}
