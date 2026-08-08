// Unicode normalization for consistent scanning.
//
// Applies NFC normalization before scanning so that identical-looking text
// with different byte representations produces identical diagnostics.
// Returns the normalized text and a byte-offset mapping from normalized
// positions back to original positions.

use std::borrow::Cow;

use unicode_normalization::{IsNormalized, UnicodeNormalization};

/// Result of NFC normalization with offset mapping.
pub struct Normalized<'a> {
    /// NFC-normalized text. Borrows the original when already NFC.
    pub text: Cow<'a, str>,
    /// Maps each byte index in text to the corresponding byte index in the
    /// original input. Length equals text.len() + 1 (the extra entry maps
    /// the end-of-string position).
    ///
    /// Empty when text is already NFC (identity mapping). Use map_offset
    /// which handles the empty case by returning the input offset unchanged.
    pub offset_map: Vec<usize>,
}

/// Normalize input to NFC, returning the normalized text and a byte-offset
/// mapping back to the original.
///
/// If the input is already in NFC, the mapping is an identity (each index
/// maps to itself). When normalization changes character boundaries, the
/// mapping tracks how each normalized byte relates to the original.
pub fn normalize_nfc(input: &str) -> Normalized<'_> {
    // Fast path: quick-check first so common clean zh-TW input avoids the
    // full normalization walk and any allocation.  Fall back to the exact
    // check only for the indeterminate Maybe case; skip it entirely when
    // the answer is already definitive (Yes or No).
    match unicode_normalization::is_nfc_quick(input.chars()) {
        IsNormalized::Yes => {
            return Normalized {
                text: Cow::Borrowed(input),
                offset_map: Vec::new(),
            };
        }
        IsNormalized::Maybe if unicode_normalization::is_nfc(input) => {
            return Normalized {
                text: Cow::Borrowed(input),
                offset_map: Vec::new(),
            };
        }
        _ => {} // No or Maybe-and-not-NFC: proceed to normalize below
    }

    // Normalize the full string at once (NFC requires seeing combining
    // sequences in context, not per-char).
    let nfc_text: String = input.nfc().collect();

    // Build offset mapping by aligning original and normalized chars.
    // Strategy: walk both char sequences in parallel. Each NFC output char
    // originated from one or more original chars. We map output bytes to
    // the original byte position of the first contributing char.
    let mut offset_map = Vec::with_capacity(nfc_text.len() + 1);

    let orig_chars: Vec<(usize, char)> = input.char_indices().collect();
    let nfc_chars: Vec<char> = nfc_text.chars().collect();

    let mut orig_idx = 0;

    for &nfc_char in &nfc_chars {
        // 1. Skip combining marks in 'orig' that were absorbed/reordered
        // (i.e., don't match current nfc_char).
        while orig_idx < orig_chars.len() {
            let ch = orig_chars[orig_idx].1;
            if is_combining_mark(ch) {
                if ch == nfc_char {
                    // Found a combining mark that matches current NFC char.
                    // Stop skipping. It will be consumed as the "base" of this mapping.
                    break;
                }
                // Combining mark that doesn't match. Must be absorbed/reordered.
                // Skip it.
                orig_idx += 1;
            } else {
                // Found a base char. Stop skipping.
                break;
            }
        }

        // 2. Map current nfc_char to whatever orig_idx is pointing at.
        let orig_byte = if orig_idx < orig_chars.len() {
            orig_chars[orig_idx].0
        } else {
            input.len()
        };

        // Map each byte of this NFC char to the original position.
        let char_len = nfc_char.len_utf8();
        for _ in 0..char_len {
            offset_map.push(orig_byte);
        }

        // 3. Consume the char at orig_idx (Base or Matching Combining) and
        //    any combining marks that were absorbed into it by NFC composition.
        //    The NFD decomposition of nfc_char tells us how many original chars
        //    it consumed: nfd_count − 1 combining marks follow the base.
        if orig_idx < orig_chars.len() {
            orig_idx += 1;
            let absorbed = std::iter::once(nfc_char).nfd().count().saturating_sub(1);
            for _ in 0..absorbed {
                if orig_idx < orig_chars.len() && is_combining_mark(orig_chars[orig_idx].1) {
                    orig_idx += 1;
                } else {
                    break;
                }
            }
        }
    }

    // End-of-string sentinel.
    offset_map.push(input.len());

    Normalized {
        text: Cow::Owned(nfc_text),
        offset_map,
    }
}

/// Returns true if ch is a Unicode combining mark (general categories M*).
fn is_combining_mark(ch: char) -> bool {
    // Combining Diacritical Marks: U+0300..U+036F
    // Combining Diacritical Marks Extended: U+1AB0..U+1AFF
    // Combining Diacritical Marks Supplement: U+1DC0..U+1DFF
    // Combining Diacritical Marks for Symbols: U+20D0..U+20FF
    // Combining Half Marks: U+FE20..U+FE2F
    matches!(ch,
        '\u{0300}'..='\u{036F}' |
        '\u{1AB0}'..='\u{1AFF}' |
        '\u{1DC0}'..='\u{1DFF}' |
        '\u{20D0}'..='\u{20FF}' |
        '\u{FE20}'..='\u{FE2F}'
    )
}

/// Map a byte offset in normalized text back to the original text.
///
/// When offset_map is empty (identity mapping from NFC fast path),
/// the offset is returned unchanged. Otherwise, out-of-bounds offsets
/// are clamped to the original text length.
pub fn map_offset(offset_map: &[usize], normalized_offset: usize) -> usize {
    if offset_map.is_empty() {
        return normalized_offset;
    }
    if normalized_offset >= offset_map.len() {
        *offset_map.last().unwrap_or(&0)
    } else {
        offset_map[normalized_offset]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_nfc_identity() {
        let input = "Hello 你好世界";
        let norm = normalize_nfc(input);
        assert_eq!(&*norm.text, input);
        // Fast path: empty offset_map means identity mapping.
        assert!(norm.offset_map.is_empty());
        for i in 0..=input.len() {
            assert_eq!(map_offset(&norm.offset_map, i), i);
        }
    }

    #[test]
    fn nfc_composed_vs_decomposed() {
        // U+0065 U+0301 (e + combining acute) -> U+00E9 (é precomposed).
        let decomposed = "e\u{0301}";
        let norm = normalize_nfc(decomposed);
        assert_eq!(norm.text, "\u{00E9}"); // NFC form: é
        assert_eq!(norm.text.len(), 2); // é is 2 UTF-8 bytes
                                        // The normalized é maps back to byte 0 (the 'e' position).
        assert_eq!(map_offset(&norm.offset_map, 0), 0);
        // End sentinel maps to original end.
        assert_eq!(
            map_offset(&norm.offset_map, norm.text.len()),
            decomposed.len()
        );
    }

    #[test]
    fn nfc_with_surrounding_text() {
        // "ae\u{0301}b" -> "aéb" after NFC.
        let input = "ae\u{0301}b";
        let norm = normalize_nfc(input);
        assert_eq!(norm.text, "a\u{00E9}b");
        // 'a' at norm byte 0 maps to orig byte 0.
        assert_eq!(map_offset(&norm.offset_map, 0), 0);
        // 'é' at norm byte 1 maps to orig byte 1 (the 'e').
        assert_eq!(map_offset(&norm.offset_map, 1), 1);
        // 'b' at norm byte 3 maps to orig byte 4 (after e + combining = 3 bytes).
        assert_eq!(map_offset(&norm.offset_map, 3), 4);
    }

    #[test]
    fn cjk_text_unchanged() {
        let input = "繁體中文測試";
        let norm = normalize_nfc(input);
        assert_eq!(norm.text, input);
    }

    #[test]
    fn mixed_content() {
        // Mix of ASCII, CJK, and precomposed chars - all already NFC.
        let input = "Hello 你好 café";
        let norm = normalize_nfc(input);
        assert_eq!(norm.text, input);
    }

    #[test]
    fn map_offset_out_of_bounds() {
        // For NFC fast path, map_offset returns the input offset unchanged.
        let input = "abc";
        let norm = normalize_nfc(input);
        assert_eq!(map_offset(&norm.offset_map, 100), 100);
        // For non-NFC input, map_offset clamps to original length.
        let decomposed = "e\u{0301}";
        let norm2 = normalize_nfc(decomposed);
        assert_eq!(map_offset(&norm2.offset_map, 100), decomposed.len());
    }

    #[test]
    fn empty_input() {
        let norm = normalize_nfc("");
        assert_eq!(&*norm.text, "");
        // Empty string is NFC, so fast path: empty offset_map.
        assert!(norm.offset_map.is_empty());
    }

    #[test]
    fn stacked_combining_marks_offset() {
        // "a + U+0301 + U+0301" → NFC is "á + U+0301" (first mark absorbed).
        // The remaining U+0301 in NFC output must map to byte 3 in the original
        // (the second mark), not byte 1 (the first, absorbed mark).
        //
        // Original bytes: a(0), U+0301(1-2), U+0301(3-4)  — total 5 bytes
        // NFC bytes:       á(0-1), U+0301(2-3)             — total 4 bytes
        let input = "a\u{0301}\u{0301}";
        assert_eq!(input.len(), 5); // a=1, U+0301=2, U+0301=2
        let norm = normalize_nfc(input);
        // NFC: á (U+00E9 = 2 bytes) + U+0301 (2 bytes)
        assert_eq!(norm.text.len(), 4);
        // The á at NFC byte 0 maps to orig byte 0 (the 'a').
        assert_eq!(map_offset(&norm.offset_map, 0), 0);
        // The remaining U+0301 at NFC byte 2 maps to orig byte 3 (second mark).
        assert_eq!(map_offset(&norm.offset_map, 2), 3);
    }
}
