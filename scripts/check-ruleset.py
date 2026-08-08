#!/usr/bin/env python3
"""Check, deduplicate, sort, and compact-format assets/ruleset.json.

- spelling_rules: unique by "from", sorted by "from"
- case_rules: unique by "term", sorted by "term"
- First occurrence wins when duplicates exist
- Short arrays (single-element to/alternatives) are kept on one line
- Detects semantic conflicts between spelling rules (--lint)
- Online verification of to-terms via Wikipedia/zh and MoE dict (--verify)
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any


def dedup_sort(
    rules: list[dict[str, Any]], key: str
) -> tuple[list[dict[str, Any]], list[str]]:
    """Deduplicate by exact key and by space-normalized key, then sort.

    Two rules whose key values differ only by whitespace (e.g.
    "標準 C 庫" vs "標準C庫") are treated as duplicates — the first
    occurrence wins.

    Returns (deduplicated_rules, space_dup_warnings).
    """
    seen: set[str] = set()
    seen_nospace: dict[str, str] = {}  # nospace -> first key
    out: list[dict[str, Any]] = []
    space_warnings: list[str] = []
    for rule in rules:
        k = rule[key]
        if k in seen:
            continue
        ns = k.replace(" ", "").replace("\u3000", "")
        if ns in seen_nospace:
            space_warnings.append(
                f'space-dup: "{k}" collapsed as duplicate of '
                f'"{seen_nospace[ns]}" (differ only by whitespace)'
            )
            continue
        seen.add(k)
        seen_nospace[ns] = k
        out.append(rule)
    return sorted(out, key=lambda r: r[key]), space_warnings


# Valid rule types (must match RuleType enum in src/rules/ruleset.rs).
VALID_RULE_TYPES = {
    "cross_strait",
    "variant",
    "typo",
    "confusable",
    "political_coloring",
    "ai_filler",
    "translationese",
}

# All known spelling rule fields (anything else is an unknown key warning).
KNOWN_SPELLING_FIELDS = {
    "from",
    "to",
    "type",
    "disabled",
    "context",
    "english",
    "exceptions",
    "context_clues",
    "negative_context_clues",
    "positional_clues",
    "tags",
    "editorial_confidence",
}

# Field order for spelling rules (stable, human-scannable output).
SPELLING_FIELD_ORDER = [
    "from",
    "to",
    "type",
    "disabled",
    "context",
    "english",
    "context_clues",
    "negative_context_clues",
    "positional_clues",
    "exceptions",
    "tags",
    "editorial_confidence",
]

CASE_FIELD_ORDER = ["term", "alternatives", "disabled"]


def ordered_rule(rule: dict[str, Any], order: list[str]) -> dict[str, Any]:
    """Return a dict with keys in the specified order, extras appended."""
    out: dict[str, Any] = {}
    for k in order:
        if k in rule:
            out[k] = rule[k]
    for k in rule:
        if k not in out:
            out[k] = rule[k]
    return out


def format_rule(rule: dict[str, Any], base: str = "    ") -> str:
    """Format a single rule object with compact arrays."""
    inner = base + "  "
    lines = [base + "{"]
    items = list(rule.items())
    for i, (key, value) in enumerate(items):
        comma = "," if i < len(items) - 1 else ""
        val_str = json.dumps(value, ensure_ascii=False)
        lines.append(f'{inner}"{key}": {val_str}{comma}')
    lines.append(base + "}")
    return "\n".join(lines)


def format_ruleset(data: dict[str, Any]) -> str:
    """Format the entire ruleset with compact rule objects."""
    parts = ["{"]

    for section_idx, (section_key, order) in enumerate(
        [
            ("spelling_rules", SPELLING_FIELD_ORDER),
            ("case_rules", CASE_FIELD_ORDER),
        ]
    ):
        rules = data[section_key]
        parts.append(f'  "{section_key}": [')
        for i, rule in enumerate(rules):
            ordered = ordered_rule(rule, order)
            comma = "," if i < len(rules) - 1 else ""
            rule_str = format_rule(ordered)
            if comma:
                # Append comma to the closing brace line
                rule_str = rule_str[:-1] + "},"
            parts.append(rule_str)
        section_comma = "," if section_idx == 0 else ""
        parts.append(f"  ]{section_comma}")

    parts.append("}")
    return "\n".join(parts) + "\n"


# ---------------------------------------------------------------------------
# Online verification helpers (--verify)
# ---------------------------------------------------------------------------

_HTTP_HEADERS = {"User-Agent": "zhtw-mcp-check/1.0"}
_RATE_LIMIT = 0.25  # seconds between requests
# Bump when the lookup algorithm changes to invalidate stale entries.
_CACHE_VERSION = 2

# Sentinel: distinguish "confirmed missing" from "network error".
_NETWORK_ERROR = object()


def _http_get_json(url: str) -> dict[str, Any] | None:
    """GET *url* and return parsed JSON, None on 404, _NETWORK_ERROR otherwise."""
    req = urllib.request.Request(url, headers=_HTTP_HEADERS)
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            data = json.loads(resp.read())
            if not isinstance(data, dict):
                return _NETWORK_ERROR  # type: ignore[return-value]
            return data
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None  # confirmed missing
        return _NETWORK_ERROR  # type: ignore[return-value]
    except Exception:
        return _NETWORK_ERROR  # type: ignore[return-value]


def wiki_zh_exists(term: str) -> tuple[bool | None, str | None]:
    """Check whether *term* is a recognized zh-TW term on zh.wikipedia.org.

    Two-tier lookup:
    1. Title match with zh-TW variant conversion (strong: dedicated page).
    2. Full-text search fallback (weaker: term appears in any article).

    Returns (True, title), (False, None) for confirmed missing,
    or (None, None) on network error.
    """
    # Tier 1: exact title with zh-TW variant conversion.
    params = urllib.parse.urlencode(
        {
            "action": "query",
            "titles": term,
            "format": "json",
            "redirects": "1",
            "converttitles": "1",
            "variant": "zh-tw",
        }
    )
    data = _http_get_json(f"https://zh.wikipedia.org/w/api.php?{params}")
    if data is _NETWORK_ERROR:
        return None, None
    if data:
        pages = data.get("query", {}).get("pages", {})
        for pid, page in pages.items():
            if "missing" not in page:
                return True, page.get("title")

    # Tier 2: search — does the term appear anywhere in zh Wikipedia?
    params = urllib.parse.urlencode(
        {
            "action": "query",
            "list": "search",
            "srsearch": term,
            "format": "json",
            "srlimit": "1",
            "srnamespace": "0",
        }
    )
    time.sleep(_RATE_LIMIT)
    data = _http_get_json(f"https://zh.wikipedia.org/w/api.php?{params}")
    if data is _NETWORK_ERROR:
        return None, None
    if not data:
        return False, None
    hits = data.get("query", {}).get("search", [])
    if hits:
        return True, hits[0].get("title")
    return False, None


def moedict_exists(word: str) -> tuple[bool | None, str | None]:
    """Look up *word* in the MoE Revised Mandarin Dictionary (moedict.tw).

    Returns (True, title), (False, None) for confirmed missing,
    or (None, None) on network error.
    """
    encoded = urllib.parse.quote(word)
    data = _http_get_json(f"https://www.moedict.tw/a/{encoded}.json")
    if data is _NETWORK_ERROR:
        return None, None
    if not data:
        return False, None
    title = data.get("t", "").replace("`", "").replace("~", "")
    return True, title or word


def _load_cache(cache_path: Path) -> dict[str, Any]:
    if cache_path.exists():
        try:
            data = json.loads(cache_path.read_text(encoding="utf-8"))
            if not isinstance(data, dict):
                print("  cache: unexpected format, discarding", file=sys.stderr)
                return {"version": _CACHE_VERSION, "wiki": {}, "moedict": {}}
            if data.get("version") == _CACHE_VERSION:
                return data
            # Version mismatch: discard stale cache.
            print(
                f"  cache version mismatch (have {data.get('version')}, "
                f"want {_CACHE_VERSION}), discarding",
                file=sys.stderr,
            )
        except (json.JSONDecodeError, OSError):
            pass
    return {"version": _CACHE_VERSION, "wiki": {}, "moedict": {}}


def _atomic_write(path: Path, content: str) -> None:
    """Write *content* to *path* atomically via temp + rename."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(content, encoding="utf-8")
    tmp.replace(path)


def _save_cache(cache_path: Path, cache: dict[str, Any]) -> None:
    cache["version"] = _CACHE_VERSION
    _atomic_write(
        cache_path,
        json.dumps(cache, ensure_ascii=False, indent=2) + "\n",
    )


def verify_terms(
    spelling_rules: list[dict[str, Any]],
    cache_path: Path,
) -> tuple[list[str], int]:
    """Verify to-field terms online.

    1. Wikipedia/zh: check that each non-empty to-term is a recognized
       zh-TW term (page exists or redirects to one).
    2. MoE dict: for variant rules, confirm the to-character is in the
       Ministry of Education dictionary.

    Returns (warnings, net_errors).
    """
    cache = _load_cache(cache_path)
    wiki_cache: dict[str, bool] = cache.get("wiki", {})
    moe_cache: dict[str, bool] = cache.get("moedict", {})
    warnings: list[str] = []

    # Collect unique to-terms and variant to-terms.
    all_to: dict[str, list[str]] = {}  # term -> [from_keys that reference it]
    variant_to: dict[str, list[str]] = {}

    for rule in spelling_rules:
        if rule.get("disabled"):
            continue
        from_key = rule["from"]
        for target in rule.get("to", []):
            if not target:
                continue
            all_to.setdefault(target, []).append(from_key)
            if rule.get("type") == "variant":
                variant_to.setdefault(target, []).append(from_key)

    def _checkpoint() -> None:
        cache["wiki"] = wiki_cache
        cache["moedict"] = moe_cache
        _save_cache(cache_path, cache)

    net_errors = 0

    # --- Wikipedia/zh verification ---
    wiki_todo = [t for t in all_to if t not in wiki_cache]
    total_wiki = len(wiki_todo)
    if total_wiki:
        print(
            f"  Wikipedia/zh: checking {total_wiki} uncached terms ...",
            file=sys.stderr,
            flush=True,
        )
    for i, term in enumerate(wiki_todo):
        exists, _ = wiki_zh_exists(term)
        if exists is None:
            net_errors += 1  # network error -- don't cache
        else:
            wiki_cache[term] = exists
        if (i + 1) % 50 == 0:
            print(f"    ... {i + 1}/{total_wiki}", file=sys.stderr, flush=True)
            _checkpoint()
        time.sleep(_RATE_LIMIT)

    wiki_missing = [t for t in all_to if wiki_cache.get(t) is False]
    for term in sorted(wiki_missing):
        sources = ", ".join(all_to[term][:3])
        if len(all_to[term]) > 3:
            sources += f" +{len(all_to[term]) - 3} more"
        warnings.append(f'wikipedia missing: "{term}" (from: {sources})')

    # --- MoE dict verification (variant rules only) ---
    # For multi-character terms (e.g. 臺北), the variant rule validates the
    # character form, not the compound word.  If the full term is absent from
    # moedict, fall back to checking each individual character -- all must be
    # present for the term to pass.
    moe_todo = [t for t in variant_to if t not in moe_cache]
    total_moe = len(moe_todo)
    if total_moe:
        print(
            f"  MoE dict: checking {total_moe} uncached variant terms ...",
            file=sys.stderr,
            flush=True,
        )
    for i, term in enumerate(moe_todo):
        exists, _ = moedict_exists(term)
        if exists is None:
            net_errors += 1
            continue  # network error -- skip, don't cache
        if not exists and len(term) > 1:
            # Fall back: check each character individually.
            all_chars_ok = True
            skip = False
            for ch in term:
                if ch in moe_cache:
                    if not moe_cache[ch]:
                        all_chars_ok = False
                        break
                    continue
                ch_exists, _ = moedict_exists(ch)
                if ch_exists is None:
                    net_errors += 1
                    skip = True
                    break
                moe_cache[ch] = ch_exists
                time.sleep(_RATE_LIMIT)
                if not ch_exists:
                    all_chars_ok = False
                    break
            if skip:
                continue  # don't cache partial result
            exists = all_chars_ok
        moe_cache[term] = exists
        if (i + 1) % 20 == 0:
            print(f"    ... {i + 1}/{total_moe}", file=sys.stderr, flush=True)
            _checkpoint()
        time.sleep(_RATE_LIMIT)

    moe_missing = [t for t in variant_to if moe_cache.get(t) is False]
    for term in sorted(moe_missing):
        sources = ", ".join(variant_to[term][:3])
        if len(variant_to[term]) > 3:
            sources += f" +{len(variant_to[term]) - 3} more"
        warnings.append(f'moedict missing: "{term}" (variant from: {sources})')

    if net_errors:
        print(
            f"  warning: {net_errors} network errors (not cached, retry later)",
            file=sys.stderr,
        )

    # Persist cache.
    cache["wiki"] = wiki_cache
    cache["moedict"] = moe_cache
    _save_cache(cache_path, cache)

    return warnings, net_errors


# Valid @domain labels (canonical single-label taxonomy).
VALID_DOMAINS = {
    "IT",
    "UI",
    "程式設計",
    "作業系統",
    "硬體",
    "電子",
    "網路",
    "通訊",
    "資安",
    "資料結構",
    "資料庫",
    "資料",
    "雲端",
    "數學",
    "科學",
    "語言學",
    "醫學",
    "金融",
    "商業",
    "電商",
    "社群",
    "教育",
    "日常",
    "圖形",
    "航太",
    "文書",
    "版本控制",
    "系統程式",
    "軟體授權",
    "生物學",
    "能源",
    "材料",
    "線性代數",
}

# Valid @geo sub-types.
VALID_GEO_TYPES = {"country", "city", "landmark", "university"}

# Valid @person sub-types.
VALID_PERSON_TYPES = {"science", "tech", "politics"}

# Country-name terms that should be expressed as "tw"/"cn" in context
# fields per CLAUDE.md convention.  Only the bare region/country names
# are listed; legitimate compound proper nouns (國立臺灣大學, 中華民國,
# 中華人民共和國, 中國共產黨) are exempted by COUNTRY_TERM_EXEMPT_PHRASES.
COUNTRY_TERMS_IN_CONTEXT = ["台灣", "臺灣", "中國大陸", "大陸", "中國"]

# Compound proper nouns where a region name appearing in a context field
# is legitimate (the term is part of a fixed name, not a region marker).
COUNTRY_TERM_EXEMPT_PHRASES = [
    "國立臺灣大學",
    "國立台灣大學",
    "中華民國",
    "中華人民共和國",
    "中國共產黨",
    "中國國民黨",
    "中華臺北",
    "中華台北",
]

# Boilerplate IT clues often get copy-pasted onto unrelated rules.
STOCK_IT_CONTEXT_CLUES = ["程式", "軟體", "系統", "電腦", "網路"]
TECHNICAL_DOMAINS_WITH_STOCK_IT_CLUES = {
    "IT",
    "UI",
    "作業系統",
    "資料",
    "資料庫",
    "資料結構",
    "硬體",
    "程式設計",
    "網路",
    "軟體授權",
    "通訊",
    "雲端",
    "電子",
}

# Common Simplified Chinese → Traditional Chinese single-char pairs.
# Used by checks 23/23b to detect SC characters in non-variant rules.
COMMON_SC2TC: dict[str, str] = {
    "儿": "兒",
    "车": "車",
    "长": "長",
    "门": "門",
    "开": "開",
    "见": "見",
    "贝": "貝",
    "气": "氣",
    "电": "電",
    "写": "寫",
    "学": "學",
    "对": "對",
    "时": "時",
    "头": "頭",
    "机": "機",
    "线": "線",
    "钱": "錢",
    "间": "間",
    "话": "話",
    "认": "認",
    "识": "識",
    "边": "邊",
    "过": "過",
    "运": "運",
    "进": "進",
    "远": "遠",
    "连": "連",
    "选": "選",
    "达": "達",
    "还": "還",
    "这": "這",
    "设": "設",
    "计": "計",
    "让": "讓",
    "议": "議",
    "记": "記",
    "许": "許",
    "论": "論",
    "语": "語",
    "说": "說",
    "请": "請",
    "读": "讀",
    "课": "課",
    "调": "調",
    "转": "轉",
    "软": "軟",
    "输": "輸",
    "农": "農",
    "邮": "郵",
    "钟": "鐘",
    "铁": "鐵",
    "银": "銀",
    "错": "錯",
    "键": "鍵",
    "镜": "鏡",
    "页": "頁",
    "顾": "顧",
    "显": "顯",
    "风": "風",
    "飞": "飛",
    "饭": "飯",
    "馆": "館",
    "马": "馬",
    "驱": "驅",
    "验": "驗",
    "龙": "龍",
    "园": "園",
    "网": "網",
    "络": "絡",
    "点": "點",
    "视": "視",
    "频": "頻",
    "审": "審",
    "广": "廣",
    "应": "應",
    "录": "錄",
    "态": "態",
    "总": "總",
    "据": "據",
    "数": "數",
    "断": "斷",
    "无": "無",
    "术": "術",
    "条": "條",
    "构": "構",
    "标": "標",
    "检": "檢",
    "毕": "畢",
    "测": "測",
    "热": "熱",
    "现": "現",
    "产": "產",
    "盘": "盤",
    "监": "監",
    "码": "碼",
    "确": "確",
    "种": "種",
    "笔": "筆",
    "类": "類",
    "级": "級",
    "组": "組",
    "经": "經",
    "结": "結",
    "给": "給",
    "统": "統",
    "维": "維",
    "缓": "緩",
    "编": "編",
    "脑": "腦",
    "节": "節",
    "药": "藥",
    "营": "營",
    "获": "獲",
    "规": "規",
    "观": "觀",
    "触": "觸",
    "证": "證",
    "评": "評",
    "资": "資",
    "与": "與",
    "专": "專",
    "业": "業",
    "两": "兩",
    "严": "嚴",
    "丰": "豐",
    "临": "臨",
    "为": "為",
    "举": "舉",
    "义": "義",
    "书": "書",
    "买": "買",
    "争": "爭",
    "亿": "億",
    "从": "從",
    "仓": "倉",
    "价": "價",
    "众": "眾",
    "优": "優",
    "传": "傳",
    "体": "體",
    "们": "們",
    "关": "關",
    "养": "養",
    "决": "決",
    "净": "淨",
    "准": "準",
    "击": "擊",
    "创": "創",
    "别": "別",
    "办": "辦",
    "务": "務",
    "动": "動",
    "区": "區",
    "医": "醫",
    "华": "華",
    "单": "單",
    "卖": "賣",
    "卫": "衛",
    "厂": "廠",
    "历": "歷",
    "厅": "廳",
    "压": "壓",
    "变": "變",
    "号": "號",
    "叶": "葉",
    "听": "聽",
    "员": "員",
    "问": "問",
    "阅": "閱",
    "阳": "陽",
    "队": "隊",
    "际": "際",
    "阶": "階",
    "陈": "陳",
    "陆": "陸",
    "险": "險",
    "随": "隨",
    "隐": "隱",
    "难": "難",
    "预": "預",
    "饮": "飲",
    "桥": "橋",
    "装": "裝",
    "简": "簡",
    "离": "離",
    "独": "獨",
    "团": "團",
    "岁": "歲",
    "炉": "爐",
}


def detect_conflicts(
    spelling_rules: list[dict[str, Any]],
) -> tuple[list[str], list[str]]:
    """Detect semantic conflicts between spelling rules.

    Skips disabled rules.  Returns (errors, advisories) where errors fail
    --lint and advisories are printed but do not cause failure.

    Errors (checks 1-13, 17-20):
    1.  Circular mappings (to of rule A is from of rule B)
    2.  Empty to without english fallback (+ ai_filler to:[] must not have english)
    3.  Variant rule invariants (single non-empty to)
    4.  Orphaned seealso references in context fields
    5.  AC compound decomposition conflicts (individual rules would produce
        wrong output for a compound term that lacks its own rule)
    6.  Suggestion-is-from conflicts (a rule's to[] value is another rule's
        from, creating unintended re-flagging)
    7.  Schema validation (required fields, valid types, unknown keys)
    8.  Compound suffix preservation (longer rules must not drop suffixes
        that the base rule would preserve)
    9.  context_clues / negative_context_clues field validation
    9b. Negative clue self-suppression (neg clue == from term exactly)
    10. Self-referencing to (from value appears in its own to array)
    11. Annotation validation (@domain/@geo/@person tag format and coverage)
    12. Redundant domain constraint (限X語境 duplicates @domain X)
    13. Ungated domain constraint (限...語境 without context_clues/exceptions)
    17. ai_filler trailing punctuation (scanner handles it automatically)
    18. Negative/positive clue substring overlap (suppression bug)
    19. Exception validity (exception must contain from as substring)
    20. Contradictory positional clues (before + not_before on same term)
    21. Space-only duplicate rules (handled by dedup_sort, reported via
        its return value and appended to conflicts in main)
    23. SC char in non-variant rules (pure SC→TC and mixed SC in from)
    24. Stock IT clues on non-technical domains (copy-paste smell)
    25. Country-name convention in context (use "tw"/"cn", not 台灣/中國/大陸)

    Advisories (checks 14-16, 22, 24, informational only):
    14. context_clues / negative_context_clues length convention (<=6 chars)
    15. Missing english field on cross_strait / confusable / typo rules
    16. Missing context field on cross_strait / confusable / typo / political_coloring rules
    22. Context parroting (context repeats from/to with no added information)
    """
    warnings: list[str] = []

    # All rules (including disabled) for seealso reference validation.
    all_from: dict[str, dict[str, Any]] = {r["from"]: r for r in spelling_rules}
    # Active-only for structural checks (cycles, empty-to, variant invariants).
    from_set: dict[str, dict[str, Any]] = {
        k: v for k, v in all_from.items() if not v.get("disabled")
    }

    # 1. Circular: detect actual cycles (A→B→...→A) in to→from chains.
    #    A chain A→B→C that terminates (C∉from_set) is fine — converges.
    #    Only A→B→...→A (cycle) means zh_check fix mode never converges.
    reported: set[str] = set()
    for rule in from_set.values():
        start = rule["from"]
        if start in reported:
            continue
        stack = [(start, [start])]
        found = False
        while stack and not found:
            node, path = stack.pop()
            node_rule = from_set.get(node)
            if not node_rule:
                continue
            for target in node_rule.get("to", []):
                if target == start:
                    # Context-gated cycles are safe: if every rule in the
                    # cycle has context_clues, the rules have mutually
                    # exclusive firing conditions and fix mode converges.
                    cycle_rules = [from_set[n] for n in path if n in from_set]
                    all_gated = all(r.get("context_clues") for r in cycle_rules)
                    if all_gated:
                        reported.update(path)
                        found = True
                        break
                    cycle = path + [target]
                    chain = " -> ".join(f'"{p}"' for p in cycle)
                    warnings.append(f"circular: {chain}")
                    reported.update(path)
                    found = True
                    break
                if target in from_set and target not in set(path):
                    stack.append((target, path + [target]))

    # 2. Empty to requires non-empty english (use English form convention).
    #    Exception: ai_filler rules use empty to intentionally (deletion).
    #    2b. ai_filler rules with to:[] must NOT have english — the fallback
    #        in effective_suggestions() would turn english into a suggestion,
    #        breaking the "flag-only, no suggestion" semantics.
    for rule in from_set.values():
        targets = [t for t in rule.get("to", []) if t]
        if not targets:
            if rule.get("type") == "translationese":
                # Translationese rules use empty to intentionally when the
                # pattern is report-only (e.g. G5 們 plural, V5 一個 redundancy).
                continue
            if rule.get("type") == "ai_filler":
                # ai_filler to:[] must not have english (phantom suggestion bug)
                if rule.get("to") == [] and rule.get("english"):
                    warnings.append(
                        f'ai-filler-english: "{rule["from"]}" has to:[] '
                        f"with english field — effective_suggestions() "
                        f"would use english as suggestion, breaking "
                        f'flag-only semantics; remove english or use to:[""]'
                    )
                continue
            english = rule.get("english", "")
            if not english:
                warnings.append(
                    f'empty to without english: "{rule["from"]}" '
                    f'needs "english" as fallback'
                )

    # 3. Variant rules: must have single non-empty to.
    for rule in from_set.values():
        if rule.get("type") == "variant":
            targets = [t for t in rule.get("to", []) if t]
            if len(targets) != 1:
                warnings.append(
                    f'variant to count: "{rule["from"]}" has {len(targets)} '
                    f"non-empty to entries, expected exactly 1"
                )

    # 4. Orphaned seealso (check against all rules, including disabled).
    for rule in from_set.values():
        ctx = rule.get("context", "")
        for m in re.finditer(r"\(@seealso\s+([^)]+)\)", ctx):
            for ref_name in m.group(1).split(","):
                ref_name = ref_name.strip()
                if ref_name and ref_name not in all_from:
                    warnings.append(
                        f'orphan seealso: "{rule["from"]}" references '
                        f'"{ref_name}" (not found)'
                    )

    # 5. AC compound decomposition: detect multi-char 'from' patterns whose
    #    individual characters are each 'from' keys of other rules.  Without
    #    a dedicated compound rule, LeftmostLongest AC may match the shorter
    #    individual rules and produce concatenated gibberish.
    #    Example: 堆棧 without its own rule → 堆→堆積 + 棧→堆疊 = 堆積堆疊
    single_char_from: dict[str, str] = {}
    for rule in from_set.values():
        if len(rule["from"]) == 1:
            targets = [t for t in rule.get("to", []) if t]
            if targets:
                single_char_from[rule["from"]] = targets[0]
    for rule in from_set.values():
        frm = rule["from"]
        if len(frm) < 2:
            continue
        # Check if every character in 'from' is itself a single-char rule.
        decomposable_chars = [ch for ch in frm if ch in single_char_from]
        if len(decomposable_chars) >= 2 and len(decomposable_chars) == len(frm):
            # The compound has a rule — good.  But check if its 'to' would
            # differ from naively concatenating individual replacements.
            naive = "".join(single_char_from[ch] for ch in frm)
            targets = [t for t in rule.get("to", []) if t]
            if targets and targets[0] != naive:
                # This is fine — the compound rule overrides the naive result.
                pass
            elif not targets:
                warnings.append(
                    f'compound decomposition: "{frm}" has no to[] but '
                    f'individual rules would produce "{naive}"'
                )
    # Also check that existing compound rules whose 'from' can be
    # decomposed into single-char rules have correct 'to' values
    # (i.e., the compound rule isn't accidentally doing the same thing
    # as naive concatenation when it shouldn't, or vice versa).
    # We intentionally do NOT enumerate all possible 2-char pairs — that
    # produces a noisy cartesian product.  Instead we rely on the compound
    # decomposition check above for existing rules and on manual review
    # for new compound terms.

    # 6. Suggestion-is-from: a rule's to[] value is another active rule's
    #    from key.  This means applying fix mode once leaves a term that
    #    will be re-flagged on the next scan — the fix doesn't converge in
    #    one pass.  Chains that terminate (A→B, B→C, C∉from) are fine
    #    (caught by circular check above).  Flag single-hop re-flagging.
    for rule in from_set.values():
        for target in rule.get("to", []):
            if not target:
                continue
            if target in from_set and target != rule["from"]:
                target_rule = from_set[target]
                # Skip if the target rule fires only conditionally — either
                # positive context_clues (which must be present for it to
                # fire) or negative_context_clues (which suppress it in the
                # matching context).  In both cases the re-flag is not
                # guaranteed, so the chain is not an unconditional 2-pass
                # convergence hazard.
                if target_rule.get("context_clues") or target_rule.get(
                    "negative_context_clues"
                ):
                    continue
                target_to = [t for t in target_rule.get("to", []) if t]
                if target_to:
                    warnings.append(
                        f'suggestion-is-from: "{rule["from"]}" suggests '
                        f'"{target}" which is from of another rule '
                        f"(→ {target_to[0]}); fix mode needs 2 passes"
                    )

    # 7. Schema validation: required fields, valid types, unknown keys.
    for rule in spelling_rules:
        frm = rule.get("from")
        if not frm:
            warnings.append("schema: rule missing required 'from' field")
            continue
        if "to" not in rule:
            warnings.append(f"schema: \"{frm}\" missing required 'to' field")
        if "type" not in rule:
            warnings.append(f"schema: \"{frm}\" missing required 'type' field")
        else:
            rtype = rule["type"]
            if rtype not in VALID_RULE_TYPES:
                warnings.append(
                    f'schema: "{frm}" has unknown type "{rtype}" '
                    f"(valid: {', '.join(sorted(VALID_RULE_TYPES))})"
                )
        unknown = set(rule.keys()) - KNOWN_SPELLING_FIELDS
        if unknown:
            warnings.append(f'schema: "{frm}" has unknown fields: {sorted(unknown)}')
        # Validate positional_clues syntax (operator:term).
        VALID_POSITIONAL_OPS = (
            "before:",
            "after:",
            "adjacent:",
            "not_before:",
            "not_after:",
        )
        pc = rule.get("positional_clues")
        if pc is not None and not isinstance(pc, list):
            warnings.append(
                f'schema: "{frm}" positional_clues must be a list, got {type(pc).__name__}'
            )
            pc = []
        for clue in pc or []:
            if not isinstance(clue, str):
                warnings.append(
                    f'schema: "{frm}" positional_clue entries must be strings, '
                    f"got {type(clue).__name__}"
                )
                continue
            if not any(
                clue.startswith(op) and len(clue) > len(op)
                for op in VALID_POSITIONAL_OPS
            ):
                warnings.append(
                    f'schema: "{frm}" has invalid positional_clue "{clue}" '
                    f"(must be operator:term, operators: "
                    f"{', '.join(op.rstrip(':') for op in VALID_POSITIONAL_OPS)})"
                )

    # 8. Compound suffix preservation: when a longer rule A contains a
    #    shorter rule B as prefix, AND both produce the same prefix in
    #    their replacement, A must not silently drop the remaining suffix.
    #    Example: "批量處理" → "批次" is wrong (drops 處理);
    #             "批量" → "批次" + 處理 = "批次處理" is correct.
    #
    #    Whole-phrase translations where the zh-TW term is structurally
    #    different (e.g. 航天飛機→太空梭, 調製解調器→數據機) are NOT
    #    flagged — the compound replacement is a distinct lexical item.
    #    We detect this by checking if the compound's to[] starts with
    #    the base rule's to[] — if not, it's a whole-phrase replacement.
    for rule in from_set.values():
        frm = rule["from"]
        targets = [t for t in rule.get("to", []) if t]
        if not targets or len(frm) < 3:
            continue
        for base_rule in from_set.values():
            base_frm = base_rule["from"]
            if base_frm == frm or len(base_frm) >= len(frm):
                continue
            if not frm.startswith(base_frm):
                continue
            base_targets = [t for t in base_rule.get("to", []) if t]
            if not base_targets:
                continue
            suffix = frm[len(base_frm) :]
            compound_result = targets[0]
            base_to = base_targets[0]
            # Only flag if the compound's replacement shares the same
            # prefix as the base rule's replacement — this means the
            # compound is doing a prefix swap and dropping the suffix.
            # Whole-phrase replacements (different prefix) are intentional.
            if not compound_result.startswith(base_to):
                continue
            # Flag only when the compound result equals the base result
            # exactly (suffix completely discarded) or the compound result
            # is just the base_to with no suffix replacement at all.
            # Suffix transformations (文件→檔, 程序→器) are intentional.
            #
            # Exclude: when base_to already ends with the suffix, the
            # compound rule exists to absorb it and prevent doubling
            # (e.g. SQL隱碼攻擊 already contains 攻擊; 公車 contains 車).
            if compound_result == base_to and not base_to.endswith(suffix):
                # Skip when the base rule already lists the compound's
                # from term as an exception — the scanner will never apply
                # the base rule to that compound, so no conflict in practice.
                base_exceptions = base_rule.get("exceptions", [])
                if frm in base_exceptions:
                    continue
                base_result = base_to + suffix
                warnings.append(
                    f'compound-suffix: "{frm}" → "{compound_result}" '
                    f'drops suffix "{suffix}"; base rule "{base_frm}" '
                    f'→ "{base_to}" would give "{base_result}"'
                )

    # 9. context_clues / negative_context_clues validation.
    for rule in from_set.values():
        frm = rule["from"]
        for field in ("context_clues", "negative_context_clues"):
            clues = rule.get(field)
            if clues is None:
                continue
            if not isinstance(clues, list):
                warnings.append(f'clue-type: "{frm}" {field} must be a list')
                continue
            if not clues:
                warnings.append(f'clue-empty: "{frm}" has empty {field} list')
            for clue in clues:
                if not clue or not clue.strip():
                    warnings.append(f'clue-blank: "{frm}" has blank entry in {field}')
        # Overlap: same term in both positive and negative clues.
        pos = set(rule.get("context_clues") or [])
        neg = set(rule.get("negative_context_clues") or [])
        overlap = pos & neg
        if overlap:
            warnings.append(
                f'clue-overlap: "{frm}" has terms in both context_clues '
                f"and negative_context_clues: {sorted(overlap)}"
            )
        # Self-suppression: negative clue equals the from term exactly.
        # Compound negative clues that merely contain from as substring
        # (e.g. from="搜索", neg="搜索令") are intentional — they suppress
        # the rule when the compound word appears in context.
        for clue in rule.get("negative_context_clues") or []:
            if isinstance(clue, str) and clue == frm:
                warnings.append(
                    f'neg-self-suppress: "{frm}" negative_context_clue '
                    f"equals the from term (would always self-suppress)"
                )

    # 10. Self-referencing to: from value must not appear in its own to array.
    for rule in from_set.values():
        frm = rule["from"]
        if frm in rule.get("to", []):
            warnings.append(
                f'self-ref: "{frm}" appears in its own to array '
                f"(identity suggestion)"
            )

    # 11. Annotation validation: @domain, @geo, and @person tags.
    #
    # Rules that use structured annotations:
    #   @geo TYPE (LABEL)        -- geographic entities
    #   @person TYPE (LABEL)     -- person name transliterations
    #   @domain LABEL            -- domain-specific terms
    #   @domain LABEL。note      -- domain + disambiguation
    #
    # cross_strait rules must have one of: @domain, @geo, @person,
    # (@seealso ...), or compound: prefix.  Bare prose without a
    # structured tag is flagged.
    # Anchored: after the tag, only 。(note) or end-of-string is valid.
    geo_re = re.compile(r"^@geo\s+(\w+)\s*(?:\([^)]*\))?\s*(?:。|$)")
    person_re = re.compile(r"^@person\s+(\w+)\s*(?:\([^)]*\))?\s*(?:。|$)")
    domain_re = re.compile(r"^@domain\s+([^。\s]+)\s*(?:。|$)")

    for rule in from_set.values():
        frm = rule["from"]
        ctx = rule.get("context", "")
        rtype = rule.get("type", "")

        # Only cross_strait rules need annotation.
        if rtype != "cross_strait":
            continue

        # Check @geo format.
        geo_m = geo_re.match(ctx)
        if geo_m:
            geo_type = geo_m.group(1)
            if geo_type not in VALID_GEO_TYPES:
                warnings.append(
                    f'geo-type: "{frm}" has unknown @geo type '
                    f'"{geo_type}" (valid: {", ".join(sorted(VALID_GEO_TYPES))})'
                )
            continue  # has @geo -- skip further annotation checks

        # Detect malformed @geo (starts with @geo but regex didn't match).
        if ctx.startswith("@geo"):
            warnings.append(
                f'geo-malformed: "{frm}" has malformed @geo tag: ' f'"{ctx[:40]}"'
            )
            continue

        # Check @person format.
        person_m = person_re.match(ctx)
        if person_m:
            person_type = person_m.group(1)
            if person_type not in VALID_PERSON_TYPES:
                warnings.append(
                    f'person-type: "{frm}" has unknown @person type '
                    f'"{person_type}" (valid: {", ".join(sorted(VALID_PERSON_TYPES))})'
                )
            continue  # has @person -- skip further annotation checks

        # Detect malformed @person (starts with @person but regex didn't match).
        if ctx.startswith("@person"):
            warnings.append(
                f'person-malformed: "{frm}" has malformed @person tag: ' f'"{ctx[:40]}"'
            )
            continue

        # Check @domain format.
        dom_m = domain_re.match(ctx)
        if dom_m:
            domain = dom_m.group(1)
            if domain not in VALID_DOMAINS:
                warnings.append(
                    f'domain-label: "{frm}" has unknown @domain ' f'"{domain}"'
                )
            continue  # has @domain -- skip further annotation checks

        # Detect malformed @domain (starts with @domain but regex didn't match).
        if ctx.startswith("@domain"):
            warnings.append(
                f'domain-malformed: "{frm}" has malformed @domain tag: ' f'"{ctx[:40]}"'
            )
            continue

        # Other structured annotations: (@seealso ...) and compound: are
        # acceptable without a @domain/@geo/@person prefix.  Match the
        # (@seealso REF) syntax, not a bare substring.
        if "(@seealso " in ctx or ctx.startswith("compound:"):
            continue

        # cross_strait rules must have a structured annotation tag.
        # Bare prose context without @domain/@geo/@person is flagged so new
        # rules are required to declare their domain explicitly.
        warnings.append(
            f'annotation-missing: "{frm}" has no @domain/@geo/@person tag'
            + (f' (context: "{ctx[:30]}...")' if ctx.strip() else "")
        )

    # Duplicate @geo: same to[0] from multiple from values.
    # OpenCC character variants (裡/里, 羣/群, 託/托) intentionally produce
    # duplicate from→to mappings so both text forms are caught.  Only flag
    # true duplicates where the from values are identical or differ by more
    # than single-character variant swaps.
    geo_to_map: dict[str, list[str]] = {}
    for rule in from_set.values():
        ctx = rule.get("context", "")
        if not ctx.startswith("@geo"):
            continue
        targets = [t for t in rule.get("to", []) if t]
        if targets:
            geo_to_map.setdefault(targets[0], []).append(rule["from"])
    for to_val, froms in geo_to_map.items():
        if len(froms) <= 1:
            continue
        # Check if all pairs differ by only single-char substitutions
        # (OpenCC variant pairs like 裡/里).  If so, it is intentional.
        # NOTE: this is a Hamming-distance heuristic, not true OpenCC
        # normalization.  It tolerates ≤2 char diffs at equal length.
        # For geographic names this is sufficient — two unrelated countries
        # with same-length names differing by ≤2 chars is not realistic.
        is_variant_pair = True
        for i in range(len(froms)):
            for j in range(i + 1, len(froms)):
                a, b = froms[i], froms[j]
                if len(a) != len(b):
                    is_variant_pair = False
                    break
                diffs = sum(1 for x, y in zip(a, b) if x != y)
                if diffs > 2:  # allow up to 2 char differences
                    is_variant_pair = False
                    break
            if not is_variant_pair:
                break
        if not is_variant_pair:
            warnings.append(
                f'geo-duplicate: {froms} all map to "{to_val}" '
                f"(redundant @geo rules)"
            )

    # 12. Redundant domain constraint: @domain X + 限X語境 in the same
    #     context is redundant — the @domain tag already declares the domain.
    for rule in from_set.values():
        frm = rule["from"]
        ctx = rule.get("context", "")
        dom_m = domain_re.match(ctx)
        if not dom_m:
            continue
        domain = dom_m.group(1)
        if f"限{domain}語境" in ctx:
            warnings.append(
                f'domain-redundant: "{frm}" has @domain {domain} '
                f"and redundant 限{domain}語境"
            )

    # 13. Ungated domain constraint: context says 限...語境 but the rule
    #     lacks context_clues, negative_context_clues, and exceptions to
    #     enforce it.  This is a latent false-positive bug per CLAUDE.md
    #     conventions.  Rules with negative_context_clues are considered
    #     gated (they fire by default and are suppressed in wrong contexts).
    limit_re = re.compile(r"限[^。]+語境")
    for rule in from_set.values():
        frm = rule["from"]
        ctx = rule.get("context", "")
        if not limit_re.search(ctx):
            continue
        has_clues = bool(rule.get("context_clues"))
        has_neg_clues = bool(rule.get("negative_context_clues"))
        has_exceptions = bool(rule.get("exceptions"))
        if not has_clues and not has_neg_clues and not has_exceptions:
            m = limit_re.search(ctx)
            constraint = m.group(0) if m else "?"
            warnings.append(
                f'ungated-constraint: "{frm}" says "{constraint}" '
                f"but has no context_clues or exceptions"
            )

    # 18. Negative clue contained in positive clue: if a negative clue is
    #     a substring of a positive clue, then whenever the positive clue
    #     matches in text, the negative clue also matches (overlapping AC),
    #     and the negative veto kills the positive gate entirely.
    #
    #     The reverse (positive substring of negative) is intentional:
    #     the longer negative phrase is a more-specific context that
    #     should suppress the broader positive trigger.
    for rule in from_set.values():
        frm = rule["from"]
        pos = rule.get("context_clues") or []
        neg = rule.get("negative_context_clues") or []
        if not isinstance(pos, list) or not isinstance(neg, list):
            continue
        for n in neg:
            for p in pos:
                if not isinstance(n, str) or not isinstance(p, str):
                    continue
                if n in p:
                    # neg is substring of pos: pos match always triggers neg
                    warnings.append(
                        f'clue-neg-in-pos: "{frm}" neg_clue "{n}" is '
                        f'substring of pos_clue "{p}" (pos is always '
                        f"suppressed)"
                    )

    # 19. Exception validity: each exception string must contain the rule's
    #     from as a substring (otherwise the exception can never match).
    for rule in from_set.values():
        frm = rule["from"]
        for exc in rule.get("exceptions", []):
            if not isinstance(exc, str):
                continue
            if frm not in exc:
                warnings.append(
                    f'exception-invalid: "{frm}" exception "{exc}" '
                    f"does not contain from (can never match)"
                )

    # 20. Contradictory positional clues: before:X + not_before:X on the
    #     same rule is a logical contradiction (rule can never fire).
    for rule in from_set.values():
        frm = rule["from"]
        pcs = rule.get("positional_clues") or []
        if not isinstance(pcs, list):
            continue
        by_op: dict[str, set[str]] = {}
        for pc in pcs:
            if not isinstance(pc, str) or ":" not in pc:
                continue
            op, term = pc.split(":", 1)
            by_op.setdefault(op, set()).add(term)
        for pos_op, neg_op in [("before", "not_before"), ("after", "not_after")]:
            overlap = by_op.get(pos_op, set()) & by_op.get(neg_op, set())
            if overlap:
                warnings.append(
                    f'positional-contradiction: "{frm}" has both '
                    f"{pos_op} and {neg_op} for: {sorted(overlap)}"
                )

    # --- Advisory checks (14-16, 21): informational, do not fail --lint ---
    advisories: list[str] = []

    # 14. context_clues / negative_context_clues length convention.
    #     CLAUDE.md says context_clues <= 6 chars.  Longer clues still work
    #     but waste AC automaton budget and are harder to match in the
    #     40-char context window.
    MAX_CLUE_CHARS = 6
    for rule in from_set.values():
        frm = rule["from"]
        for field in ("context_clues", "negative_context_clues"):
            clues = rule.get(field)
            if not isinstance(clues, list):
                continue  # malformed; already caught by check 9
            for clue in clues:
                if not isinstance(clue, str):
                    continue  # malformed; already caught by check 9
                if len(clue) > MAX_CLUE_CHARS:
                    advisories.append(
                        f'clue-length: "{frm}" {field} entry '
                        f'"{clue}" is {len(clue)} chars (max {MAX_CLUE_CHARS})'
                    )

    # 15. Missing english field on cross_strait / confusable / typo rules.
    #     variant (single-char 異體字), ai_filler, and political_coloring
    #     rules are exempt — they either have no English equivalent or serve
    #     a non-translation purpose.
    for rule in from_set.values():
        rtype = rule.get("type", "")
        if rtype in ("variant", "ai_filler", "political_coloring", "translationese"):
            continue
        if not rule.get("english"):
            advisories.append(
                f'missing-english: "{rule["from"]}" ({rtype}) has no english field'
            )

    # 16. Missing context field on cross_strait / confusable / typo rules.
    #     variant, ai_filler, and political_coloring rules are exempt.
    for rule in from_set.values():
        rtype = rule.get("type", "")
        if rtype in ("variant", "ai_filler", "political_coloring", "translationese"):
            continue
        if not rule.get("context"):
            advisories.append(
                f'missing-context: "{rule["from"]}" ({rtype}) has no context field'
            )

    # 22. Redundant context parroting: context field that merely repeats
    #     the from/to mapping already expressed by the rule's own fields.
    #     Pattern: 'cn 用「<from>」；tw 用「<to>」' adds no information.
    parrot_re = re.compile(r"cn\s*用「([^」]*)」[；;]\s*tw\s*用「([^」]*)」")
    for rule in from_set.values():
        frm = rule["from"]
        ctx = rule.get("context", "")
        m = parrot_re.search(ctx)
        if m:
            cn_val, tw_val = m.group(1), m.group(2)
            targets = rule.get("to", [])
            if cn_val == frm and targets and tw_val == targets[0]:
                advisories.append(
                    f'context-parrot: "{frm}" context repeats from/to '
                    f"— strip redundant cn/tw text"
                )

    # 24. Stock IT clues on non-technical domains: this exact clue set is
    #     commonly copied onto unrelated rules by mistake.
    for rule in from_set.values():
        if rule.get("context_clues") != STOCK_IT_CONTEXT_CLUES:
            continue
        ctx = rule.get("context", "")
        m = re.match(r"@domain\s+([^。\s]+)", ctx)
        domain = m.group(1) if m else ""
        if domain not in TECHNICAL_DOMAINS_WITH_STOCK_IT_CLUES:
            advisories.append(
                f'stock-it-clues: "{rule["from"]}" uses stock IT clues '
                f'outside technical domains ({domain or "no @domain"})'
            )

    # 25. Country-name convention in context fields.  CLAUDE.md / project
    #     convention requires "tw"/"cn" instead of bare 台灣/臺灣/中國/大陸
    #     so context strings stay neutral and consistent.  Exemptions:
    #       - the country term appears in the rule's from/to (e.g. 中國臺灣
    #         → 臺灣 must say 臺灣 in context to disambiguate)
    #       - the country term is part of a fixed proper noun
    #         (國立臺灣大學, 中華民國, 中華人民共和國, ...)
    #       - the rule has @geo annotation (geo rules name regions)
    for rule in from_set.values():
        frm = rule["from"]
        ctx = rule.get("context", "")
        if not ctx:
            continue
        # @geo rules legitimately use region names.
        if ctx.startswith("@geo"):
            continue
        fields_text = frm + "".join(rule.get("to", []) or [])
        # Mask out exempted compound proper nouns before scanning so
        # 國立臺灣大學 does not contribute a 臺灣 hit.
        masked = ctx
        for phrase in COUNTRY_TERM_EXEMPT_PHRASES:
            if phrase in masked:
                masked = masked.replace(phrase, "_" * len(phrase))
        for term in COUNTRY_TERMS_IN_CONTEXT:
            if term in masked and term not in fields_text:
                warnings.append(
                    f'context-country-name: "{frm}" context uses "{term}" '
                    f'— project convention requires "tw"/"cn" '
                    f"(except in @geo rules or fixed proper nouns)"
                )
                break

    # 23. Simplified Chinese in non-variant rules: the 'from' field of
    #     cross_strait rules should use Traditional Chinese (the S2T
    #     converter handles SC→TC character conversion separately).
    #     Also detects rules that are purely SC→TC char substitution
    #     (from and to differ only by SC→TC character mapping).
    #
    #     Pure SC→TC rules are errors; mixed SC in 'from' is advisory
    #     (some cross_strait rules intentionally use SC to catch text
    #     after incomplete S2T conversion).
    _tc_variants = {
        r["from"]
        for r in spelling_rules
        if r.get("type") == "variant" and len(r["from"]) == 1
    }
    _sc2tc = {k: v for k, v in COMMON_SC2TC.items() if k not in _tc_variants}

    for rule in from_set.values():
        frm = rule["from"]
        rtype = rule.get("type", "")
        if rtype in ("variant", "ai_filler"):
            continue
        to = rule.get("to", [])
        to0 = to[0] if to else ""

        sc_chars = [ch for ch in frm if ch in _sc2tc]
        if not sc_chars:
            continue

        # Pure SC→TC char-level rule: same length, every diff is SC→TC.
        if len(frm) == len(to0) and frm != to0:
            all_sc2tc = all(
                _sc2tc.get(frm[i]) == to0[i]
                for i in range(len(frm))
                if frm[i] != to0[i]
            )
            if all_sc2tc:
                warnings.append(
                    f'sc-char-only: "{frm}" → "{to0}" is purely '
                    f"SC→TC character conversion (use S2T converter, "
                    f"not spelling_rules)"
                )
                continue

        # Mixed SC in 'from': error.  The S2T converter normalises SC
        # to TC before spelling rules run, so SC 'from' values never
        # match.  Use the Traditional Chinese form instead.
        warnings.append(
            f'sc-in-from: "{frm}" contains simplified character(s) '
            f'{"".join(sc_chars)} — use Traditional Chinese in from'
        )

    # 21. Space-only duplicates: handled by dedup_sort() before
    #     detect_conflicts() is called.  Warnings are reported via
    #     dedup_sort()'s return value and appended to conflicts in main().

    # 17. ai_filler trailing punctuation: the scanner extends deletion
    #     spans (is_deletion_rule: to == [""]) to consume trailing ，/：
    #     automatically.  Separate rules for phrase+punctuation variants
    #     are redundant only when the base rule is a deletion rule.
    #     Replacement ai_filler rules (to == ["總之"] etc.) do NOT get
    #     automatic trailing-punctuation handling.
    ai_filler_deletion_from = {
        r["from"]
        for r in from_set.values()
        if r.get("type") == "ai_filler"
        and len(r.get("to", [])) == 1
        and r["to"][0] == ""
    }
    for rule in from_set.values():
        if rule.get("type") != "ai_filler":
            continue
        frm = rule["from"]
        if frm.endswith("\uff0c") or frm.endswith("\uff1a"):  # ， or ：
            base = frm[:-1]
            if base in ai_filler_deletion_from:
                punct = frm[-1]
                warnings.append(
                    f'ai-filler-punct: "{frm}" is redundant — '
                    f'base rule "{base}" is a deletion rule and scanner '
                    f"auto-consumes trailing {punct}"
                )

    return warnings, advisories


def default_path() -> Path:
    return Path(__file__).resolve().parent.parent / "assets" / "ruleset.json"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", nargs="?", type=Path, default=default_path())
    parser.add_argument(
        "--lint",
        action="store_true",
        help="detect conflicts without rewriting the file (exit 1 if any)",
    )
    parser.add_argument(
        "--verify",
        action="store_true",
        help="online verification of to-terms via Wikipedia/zh and MoE dict",
    )
    parser.add_argument(
        "--cache",
        type=Path,
        default=Path(__file__).resolve().parent.parent / ".verify-cache.json",
        help="path to verification cache file (default: .verify-cache.json)",
    )
    args = parser.parse_args()
    path: Path = args.path

    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        print(f"error: file not found: {path}", file=sys.stderr)
        return 1
    except json.JSONDecodeError as e:
        print(f"error: invalid JSON in {path}: {e}", file=sys.stderr)
        return 1

    for key in ("spelling_rules", "case_rules"):
        if key not in data or not isinstance(data[key], list):
            print(f'error: missing or invalid "{key}" in {path}', file=sys.stderr)
            return 1

    orig_spelling = len(data["spelling_rules"])
    orig_case = len(data["case_rules"])

    data["spelling_rules"], space_warnings_sp = dedup_sort(
        data["spelling_rules"], "from"
    )
    data["case_rules"], space_warnings_cr = dedup_sort(data["case_rules"], "term")

    new_spelling = len(data["spelling_rules"])
    new_case = len(data["case_rules"])
    removed = (orig_spelling - new_spelling) + (orig_case - new_case)

    # Space-dup warnings from dedup_sort are treated as conflicts (errors).
    space_warnings = space_warnings_sp + space_warnings_cr

    # Detect semantic conflicts in spelling rules.
    conflicts, advisories = detect_conflicts(data["spelling_rules"])
    conflicts.extend(space_warnings)
    if conflicts:
        print(f"conflicts ({len(conflicts)}):", file=sys.stderr)
        for w in conflicts:
            print(f"  {w}", file=sys.stderr)
    if advisories and args.lint:
        print(f"advisories ({len(advisories)}):", file=sys.stderr)
        for w in advisories:
            print(f"  {w}", file=sys.stderr)

    # Online verification (opt-in).
    verify_warnings: list[str] = []
    verify_net_errors = 0
    if args.verify:
        verify_warnings, verify_net_errors = verify_terms(
            data["spelling_rules"],
            args.cache,
        )
        if verify_warnings:
            print(f"verify ({len(verify_warnings)}):", file=sys.stderr)
            for w in verify_warnings:
                print(f"  {w}", file=sys.stderr)
        else:
            print("verify: all terms confirmed", file=sys.stderr)

    if args.lint:
        print(
            f"ruleset: {new_spelling} spelling + {new_case} case"
            f" ({removed} duplicates removed)"
        )
        if conflicts or verify_warnings:
            return 1
        if verify_net_errors:
            print(
                "error: incomplete verification due to network errors", file=sys.stderr
            )
            return 1
        return 0

    _atomic_write(path, format_ruleset(data))

    print(
        f"ruleset: {new_spelling} spelling + {new_case} case"
        f" ({removed} duplicates removed)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
