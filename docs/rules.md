# Rule types

Spelling rules and case rules, organized into 8 categories.

## cross_strait

Regional terminology differences between zh-CN and zh-TW. Each rule has an `english` field for disambiguation.

| zh-CN | zh-TW | English |
|-------|-------|---------|
| 軟件 | 軟體 | software |
| 內存 | 記憶體 | memory (RAM) |
| 線程 | 執行緒 | thread |
| 進程 | 行程 | process |
| 接口 | 介面 | interface |
| 人工智能 | 人工智慧 | artificial intelligence |
| 操作系統 | 作業系統 | operating system |
| 默認 | 預設 | default |
| 代碼 | 程式碼 | code |
| \u201c / \u201d | 「 / 」 | quotation marks |

Quotation mark conversion includes a pairing fix: when CN curly quotes are unbalanced or misordered, the scanner reassigns them by alternating position (open, close, open, close).

Some cross-strait rules involve false friends (假朋友), where the `from` term is also a valid zh-TW word with a different meaning. For example, 文件 means "file" in zh-CN but "document" in zh-TW. These rules are disabled to prevent false positives.

Tree data structure terminology follows a gender-neutral naming principle (性別中立原則). English terms like "parent" and "sibling" are inherently gender-neutral, so zh-TW translations should preserve that neutrality rather than importing gendered kinship terms:

| Flagged | Suggested | English | Rationale |
|---------|-----------|---------|-----------|
| 父節點 | 親代節點 | parent node | 「親代」preserves the gender-neutral semantics of "parent" |
| 母節點 | 親代節點 | parent node | Every non-root node has exactly one parent, not a gendered pair |
| 兄弟節點 | 平輩節點 | sibling node | 「平輩」expresses same-level kinship without gender |
| 叔伯節點 | 親代的平輩節點 | uncle node | Compositional form avoids gendered kinship metaphors |

## punctuation

Context-sensitive half-width to full-width punctuation normalization for Chinese text:

| Half-width | Full-width | Condition |
|------------|------------|-----------|
| `,` | `，` | Adjacent CJK character on either side |
| `.` | `。` | Preceding CJK character (guards against decimals, file extensions, ellipsis) |
| `!` | `！` | Adjacent CJK character |
| `?` | `？` | Adjacent CJK character |
| `;` | `；` | Adjacent CJK character |
| `:` | `：` | Adjacent CJK character (exempted with `relaxed` flag) |
| `(` / `)` | `（` / `）` | Adjacent CJK character |

Also detects: CN curly quotation marks (`\u201c`/`\u201d` double, `\u2018`/`\u2019` single) with CJK adjacency guards to avoid false positives on English smart quotes and contractions (it's, don't); enumeration comma misuse (`，` where `、` is appropriate for coordinate lists); quotation mark hierarchy violations; extraneous space after full-width punctuation; and range indicator style (`～` vs `–`).

English-only contexts, thousand separators (1,000), and decimal numbers (3.14) are left untouched.

## political_coloring

Terms carrying political framing inappropriate for Taiwan contexts.

| Flagged | Suggested | English |
|---------|-----------|---------|
| 祖國 | 中國 | motherland |
| 內地 | 中國大陸 / 中國 | mainland |
| 大陸同胞 | 中國民眾 | mainland compatriots |

## confusable

Terms that are easily confused across dialects.

| Flagged | Suggested | English | Note |
|---------|-----------|---------|------|
| 字體 | 字型 | font | 字體 = typeface (design family); 字型 = font (specific size/weight instance) |

## typo

Common misspellings.

| Flagged | Suggested | English |
|---------|-----------|---------|
| 乞業 | 企業 | enterprise |

## variant

Character variant normalization per the MoE Standard Form of National Characters (國字標準字體). These map non-standard glyph forms (Kangxi, Hong Kong, generic zh-Hant) to the Taiwan standard:

| Non-standard | MoE standard | Notes |
|-------------|-------------|-------|
| 裏 | 裡 | "inside" |
| 綫 | 線 | "thread/line" |
| 麪 | 麵 | "noodle" |
| 着 | 著 | Particle usage; exception: chess term 下著, proper nouns |
| 台 | 臺 | `strict` profile only; lexical contexts: 臺灣/臺北/臺中/臺南 |

Variant rules use a separate engine pass (after spelling rules) with exception phrase checking.

## proper_noun

Country names and international organizations with cross-strait naming differences:

| zh-CN | zh-TW | English |
|-------|-------|---------|
| 老撾 | 寮國 | Laos |
| 新西蘭 | 紐西蘭 | New Zealand |
| 東盟 | 東協 | ASEAN |

## case

Proper casing for technology terms. Matched case-insensitively with word boundary checks.

```
JavaScript  TypeScript  Python  Rust  HTTP  HTTPS
API  JSON  GitHub  Instagram  Google  Facebook
React  Linux  macOS
```

## Extending the ruleset

### Adding a spelling rule

Edit `assets/ruleset.json`:

```json
{
  "from": "數據庫",
  "to": ["資料庫"],
  "type": "cross_strait",
  "context": "database = 資料庫",
  "english": "database"
}
```

Run `scripts/check-ruleset.py --lint` to validate before opening a PR.

Fields: `from` (required), `to` (required, array), `type` (required: `cross_strait` / `political_coloring` / `confusable` / `typo` / `variant`), `disabled` (optional), `context` (optional, use `@seealso` for cross-refs), `english` (optional, recommended).

### Adding a case rule

```json
{
  "term": "GraphQL",
  "alternatives": ["graphql", "GRAPHQL", "Graphql"]
}
```

### Runtime overrides

Edit `overrides.json` in the platform config directory (`~/.config/zhtw-mcp/` on Linux, `~/Library/Application Support/zhtw-mcp/` on macOS):

```json
{
  "schema_version": 3,
  "spelling": [
    {"from": "優化", "to": ["最佳化"], "type": "cross_strait", "disabled": true}
  ],
  "case": []
}
```
