#!/usr/bin/env python3
"""Generate Rust source from OpenCC dictionary text files.

Downloads STCharacters.txt, STPhrases.txt, and TWVariants.txt from the
OpenCC GitHub repository and generates a Rust source file with static
arrays that can be compiled into the binary.

This eliminates include_str! + runtime text parsing: the dictionaries
are pre-parsed into typed Rust arrays at code-generation time.

Usage:
  python3 scripts/gen-s2t-tables.py                  # download + generate
  python3 scripts/gen-s2t-tables.py --dry-run         # print stats only
  python3 scripts/gen-s2t-tables.py --check           # verify generated file is up-to-date
"""

from __future__ import annotations

import argparse
import hashlib
import sys
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DICT_DIR = REPO / "data" / "opencc"
OUTPUT = REPO / "src" / "engine" / "s2t_data.rs"

# OpenCC raw file base URL (pinned to master for reproducibility).
OPENCC_RAW = "https://raw.githubusercontent.com/BYVoid/OpenCC/master/data/dictionary"

# Dictionary files to process.
DICT_FILES = {
    "st_phrases": ("STPhrases.txt", f"{OPENCC_RAW}/STPhrases.txt"),
    "st_characters": ("STCharacters.txt", f"{OPENCC_RAW}/STCharacters.txt"),
    "tw_variants": ("TWVariants.txt", f"{OPENCC_RAW}/TWVariants.txt"),
}


def ensure_downloaded() -> None:
    """Download dictionary files from GitHub if not already cached."""
    DICT_DIR.mkdir(parents=True, exist_ok=True)
    for name, (filename, url) in DICT_FILES.items():
        path = DICT_DIR / filename
        if path.exists():
            continue
        print(f"Downloading {filename} ...", file=sys.stderr)
        try:
            urllib.request.urlretrieve(url, path)
        except Exception as e:
            print(f"error: failed to download {url}: {e}", file=sys.stderr)
            sys.exit(1)


def dict_path(name: str) -> Path:
    """Return local path for a dictionary file."""
    filename, _ = DICT_FILES[name]
    return DICT_DIR / filename


# Genuinely two-way chars curated by hand: each has two common TC readings
# (e.g. 后 後/后, 里 裡/里) so single-char fallback would guess wrong. This is a
# deliberate subset, not "every char OpenCC lists with >1 candidate" -- most of
# those (个 個, 万 萬, 当 當) have an overwhelmingly dominant reading and are safe.
MANUAL_AMBIGUOUS_CHARS = {
    "干",
    "复",
    "咸",
    "范",
    "丑",
    "佣",
    "伙",
    "舍",
    "症",
    "姜",
    "沈",
    "克",
    "后",
    "里",
    "余",
}


def parse_dict(path: Path, *, keep_identity: bool = False) -> list[tuple[str, str]]:
    """Parse a tab-separated OpenCC dictionary file.

    Returns (key, primary_value) pairs. Skips comments, empty lines,
    and, unless requested, identity mappings. For one-to-many mappings, takes
    the first value.
    """
    entries = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t", 1)
            if len(parts) != 2:
                continue
            key = parts[0]
            val = parts[1].split()[0]  # first space-separated value
            if key == val and not keep_identity:
                continue  # identity mapping
            entries.append((key, val))
    return entries


def parse_char_dict(path: Path) -> list[tuple[str, str]]:
    """Parse a character dictionary, keeping only single-char -> single-char entries."""
    entries = []
    for key, val in parse_dict(path):
        if len(key) == 1 and len(val) == 1:
            entries.append((key, val))
    return entries


def parse_ambiguous_chars() -> set[str]:
    """Return chars that must never use unconditional single-char fallback."""
    return set(MANUAL_AMBIGUOUS_CHARS)


def filter_safe_chars(
    chars: list[tuple[str, str]], ambiguous_chars: set[str]
) -> list[tuple[str, str]]:
    """Remove ambiguous chars from the single-char fallback table."""
    return [(key, val) for key, val in chars if key not in ambiguous_chars]


def parse_phrases_with_ambiguous_protection(
    path: Path, ambiguous_chars: set[str]
) -> list[tuple[str, str]]:
    """Parse STPhrases and keep identity rows only when they protect ambiguous chars."""
    entries = []
    seen = set()
    for key, val in parse_dict(path, keep_identity=True):
        if key == val and not any(ch in ambiguous_chars for ch in key):
            continue
        if key in seen:
            continue
        seen.add(key)
        entries.append((key, val))
    return entries


def compute_source_hash() -> str:
    """SHA-256 of all source dictionary files (sorted by name)."""
    h = hashlib.sha256()
    for name in sorted(DICT_FILES):
        path = dict_path(name)
        h.update(path.read_bytes())
    return h.hexdigest()[:16]


def escape_rust_str(s: str) -> str:
    """Escape a string for use in a Rust string literal."""
    return s.replace("\\", "\\\\").replace('"', '\\"')


def generate_rust(
    phrases: list[tuple[str, str]],
    chars: list[tuple[str, str]],
    ambiguous_chars: set[str],
    tw_variants: list[tuple[str, str]],
    source_hash: str,
) -> str:
    """Generate s2t_data.rs content."""
    lines = [
        "// Auto-generated by scripts/gen-s2t-tables.py — do not edit manually.",
        f"// Source hash: {source_hash}",
        "// Dictionary data: Apache-2.0 (OpenCC project).",
        "",
        "/// SC->TC phrase mappings (longest-match substitution).",
        f"/// {len(phrases)} entries from STPhrases.txt.",
        f"pub const ST_PHRASES: &[(&str, &str)] = &[",
    ]

    for key, val in phrases:
        lines.append(f'    ("{escape_rust_str(key)}", "{escape_rust_str(val)}"),')

    lines.append("];")
    lines.append("")

    lines.append(
        "/// SC->TC safe single-character mappings (fallback after phrase matching)."
    )
    lines.append(
        "/// Ambiguous one-to-many chars are deliberately excluded and fall back to identity."
    )
    lines.append(
        f"/// {len(chars)} entries from STCharacters.txt after ambiguity filtering."
    )
    lines.append(f"pub const ST_CHARACTERS: &[(char, char)] = &[")

    for key, val in chars:
        lines.append(f"    ('{escape_rust_str(key)}', '{escape_rust_str(val)}'),")

    lines.append("];")
    lines.append("")

    lines.append("/// Ambiguous SC chars excluded from ST_CHARACTERS.")
    lines.append(f"/// {len(ambiguous_chars)} entries.")
    lines.append("#[rustfmt::skip]")
    lines.append("pub const AMBIGUOUS_ST_CHARACTERS: &[char] = &[")

    for ch in sorted(ambiguous_chars):
        lines.append(f"    '{escape_rust_str(ch)}',")

    lines.append("];")
    lines.append("")

    lines.append("/// Taiwan variant normalization (applied last).")
    lines.append(f"/// {len(tw_variants)} entries from TWVariants.txt.")
    lines.append(f"pub const TW_VARIANTS: &[(char, char)] = &[")

    for key, val in tw_variants:
        lines.append(f"    ('{escape_rust_str(key)}', '{escape_rust_str(val)}'),")

    lines.append("];")
    lines.append("")

    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true", help="Print stats only")
    parser.add_argument(
        "--check",
        action="store_true",
        help="Verify generated file is up-to-date",
    )
    args = parser.parse_args()

    # Download dictionaries if needed.
    ensure_downloaded()

    # Verify dictionary files exist.
    for name in DICT_FILES:
        path = dict_path(name)
        if not path.exists():
            print(f"error: {name} not found at {path}", file=sys.stderr)
            sys.exit(1)

    ambiguous_chars = parse_ambiguous_chars()
    phrases = parse_phrases_with_ambiguous_protection(
        dict_path("st_phrases"), ambiguous_chars
    )
    chars_raw = parse_char_dict(dict_path("st_characters"))
    chars = filter_safe_chars(chars_raw, ambiguous_chars)
    tw_variants = parse_char_dict(dict_path("tw_variants"))
    source_hash = compute_source_hash()

    safe_keys = {key for key, _ in chars}
    overlap = safe_keys & ambiguous_chars
    if overlap:
        print(
            f"error: ambiguous chars leaked into ST_CHARACTERS: {sorted(overlap)}",
            file=sys.stderr,
        )
        sys.exit(1)

    print(f"STPhrases:    {len(phrases):>6} entries")
    print(f"STCharacters: {len(chars):>6} entries (safe single-char only)")
    print(f"Ambiguous:    {len(ambiguous_chars):>6} chars excluded")
    print(f"TWVariants:   {len(tw_variants):>6} entries")
    print(f"Source hash:  {source_hash}")

    if args.dry_run:
        return

    content = generate_rust(phrases, chars, ambiguous_chars, tw_variants, source_hash)

    if args.check:
        if not OUTPUT.exists():
            print(f"error: {OUTPUT} does not exist", file=sys.stderr)
            sys.exit(1)
        existing = OUTPUT.read_text(encoding="utf-8")
        if existing == content:
            print(f"OK: {OUTPUT.name} is up-to-date")
        else:
            print(
                f"error: {OUTPUT.name} is stale — run: python3 scripts/gen-s2t-tables.py",
                file=sys.stderr,
            )
            sys.exit(1)
        return

    OUTPUT.write_text(content, encoding="utf-8")
    print(f"Wrote {OUTPUT} ({len(content)} bytes)")


if __name__ == "__main__":
    main()
