use zhtw_mcp::engine::s2t::S2TConverter;

#[test]
fn ambiguous_bare_and_protected_terms_do_not_overconvert() {
    let c = S2TConverter::new();

    for (input, expected) in [
        ("范先生", "范先生"),
        ("辛丑", "辛丑"),
        ("佣金", "佣金"),
        ("天后宫", "天后宮"),
        ("邻里", "鄰里"),
        ("巧克力", "巧克力"),
    ] {
        assert_eq!(c.convert(input), expected, "{input}");
    }
}

#[test]
fn ambiguous_chars_still_convert_when_phrase_covered() {
    let c = S2TConverter::new();

    for (input, expected) in [
        ("头发", "頭髮"),
        ("面条", "麵條"),
        ("女佣", "女傭"),
        ("咸菜", "鹹菜"),
        ("芒果干", "芒果乾"),
    ] {
        assert_eq!(c.convert(input), expected, "{input}");
    }
}

// Multi-candidate chars with a dominant reading must stay in the single-char
// fallback. Over-excluding them (e.g. unioning OpenCC's raw multi-candidate
// scan) would stop these extremely common chars from converting standalone.
#[test]
fn safe_multi_candidate_chars_still_convert_standalone() {
    let c = S2TConverter::new();

    for (input, expected) in [
        ("个", "個"),
        ("万", "萬"),
        ("当", "當"),
        ("并附上", "並附上"),
    ] {
        assert_eq!(c.convert(input), expected, "{input}");
    }
}

// Excluding productive markers (里, 后) from the fallback relies on phrases to
// carry the conversion. Confirm the common phrase-covered cases still work.
#[test]
fn excluded_markers_still_convert_via_phrases() {
    let c = S2TConverter::new();

    for (input, expected) in [("以后", "以後"), ("雨后", "雨後"), ("心里", "心裡")] {
        assert_eq!(c.convert(input), expected, "{input}");
    }
}

#[test]
fn conversion_is_idempotent_for_ambiguous_cases() {
    let c = S2TConverter::new();

    for input in ["范先生", "辛丑", "佣金", "頭髮", "麵條", "芒果乾"] {
        let once = c.convert(input);
        let twice = c.convert(&once);
        assert_eq!(twice, once, "{input}");
    }
}
