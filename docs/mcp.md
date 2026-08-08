# MCP capabilities

The server exposes 1 tool, 2 resources, and 3 prompts over JSON-RPC 2.0 (stdio transport), plus MCP Sampling for server-initiated LLM disambiguation.

## Tool: `zhtw`

Unified lint / fix / gate for zh-TW text.

| Parameter | Type | Description |
|-----------|------|-------------|
| `text` | string (required) | Text to check |
| `fix_mode` | `"none"` / `"orthographic"` / `"lexical_safe"` / `"lexical_contextual"` | Fix mode (default: `"none"`) |
| `max_errors` | integer | Reject if residual errors exceed threshold |
| `max_warnings` | integer | Reject if residual warnings exceed threshold |
| `profile` | `"base"` / `"strict"` | Rule profile |
| `relaxed` | boolean | Relax colon and other UI-string-level rules |
| `content_type` | `"plain"` / `"markdown"` / `"markdown-scan-code"` / `"yaml"` | Content type (`markdown-scan-code` also lints inside code blocks) |
| `political_stance` | `"roc_centric"` / `"neutral"` / `"international"` | Political stance filter |
| `ignore_terms` | array of strings | Terms to downgrade to Info for this call |
| `explain` | boolean | Attach cultural/linguistic annotations |
| `output` | `"full"` / `"compact"` / `"tabular"` / `"summary"` | Output verbosity |
| `include_telemetry` | boolean | Include estimated token, cache, and Tier 2 resolution metrics in JSON responses (`full`, `compact`, `summary`) |

Lint only (default):

```json
{"text": "這個軟件使用了遞歸算法來遍歷鏈表"}
```

Returns issues with line/column position, matched term, suggestions, rule type, severity, and English anchor. Structured JSON responses also include document-level scan metadata when available:

- `coverage`: active spelling rules checked and distinct rules matched
- `oral_density`: spoken-style filler ratio across CJK text
- `quality_flags`: coarse document signals such as `spaced_acronyms`, `stutter_detected`, `asr_artifacts`, `high_oral_density`

The above flags: 軟件 (software), 遞歸 (recursion), 算法 (algorithm), 遍歷 (traverse), 鏈表 (linked list).

Lint + fix + gate:

```json
{"text": "請使用內存中的緩存數據", "max_errors": 0, "fix_mode": "lexical_safe"}
```

If residual errors exceed `max_errors` (or warnings exceed `max_warnings`), the response has `"accepted": false`. Otherwise `"accepted": true` with corrected text.

Per-call suppression:

```json
{"text": "這個軟件很好用", "ignore_terms": ["軟件"]}
```

Matching issues are downgraded to Info severity for this call only.

Telemetry-enabled call:

```json
{"text": "這個軟件很好用", "include_telemetry": true}
```

When enabled, the response includes a `telemetry` object with estimated prompt/completion tokens, cache hit/miss counts, Tier 2 local resolutions, and raw counters for the call. `tabular` output does not support telemetry because it is plain text rather than structured JSON.

Summary output:

```json
{"text": "這個那個這個那個這個那個這個那個這個那個", "output": "summary"}
```

Returns aggregate counts only, plus any available document-level metadata such as `coverage`, `oral_density`, `quality_flags`, and `ai_signature`.

## Resources

| URI | Description |
|-----|-------------|
| `zh-tw://style-guide/moe` | MoE punctuation, variant, and vocabulary standards (Markdown) |
| `zh-tw://dictionary/ambiguous` | Terms requiring LLM disambiguation (JSON array) |

## Prompts

| Name | Arguments | Description |
|------|-----------|-------------|
| `normalize_tone` | _(none)_ | Grounds the host LLM in MoE-standard zh-TW conventions |
| `lint_natural` | `instruction`, `text` | Translates free-form instruction into a `zhtw` tool call |
| `editorial_review` | `text`, `max_iterations` (default 3) | Iterative review: calls `zhtw`, explains issues, applies fixes until accepted |

## Sampling

When the scanner encounters an ambiguous term (with `english` field indicating multiple translations) and the client supports sampling, the server sends a `sampling/createMessage` request for LLM disambiguation. Budget: 5 calls per invocation, 5-second timeout. On timeout, the issue is kept at original severity.

## Prompt examples

Once installed, type these directly into your AI assistant's chat (Claude Code, OpenCode, etc.). The assistant will call the `zhtw` tool automatically.

### Linting and reviewing

```
Check README-zh.md for Taiwan MoE zh-TW standard violations.

Review docs/api.md for zh-CN terminology and explain each issue.

Run a strict MoE lint on this markdown and list every violation with line numbers.
```

### Auto-fixing

```
Auto-correct zh-CN vocabulary in src/locales/zh-TW.json and show the diff.

Fix all non-standard terms in CHANGELOG.md using safe mode.
Reject the result if any errors remain.
```

### Output gate (strict enforcement)

```
Lint this article with max_errors=0 and abort if any violations are found:
[paste text]

Act as a zh-TW copy editor. For every response you write in Chinese, run zhtw
with fix_mode "lexical_safe" and max_errors 0 before sending it to me.
```

### Git / CI workflows

```
Check all staged markdown files for MoE compliance before I commit.

Review every file changed in the last commit for zh-TW regressions.

Translate this English error message to Traditional Chinese, then verify with
zhtw before giving it to me.
```

### MCP prompts and resources

```
Use the normalize_tone prompt so all Chinese text you produce follows MoE standards.

Load zh-tw://style-guide/moe and follow those conventions for this session.

Use the editorial_review prompt on this draft with max_iterations=2, and stop
early if zhtw returns accepted=true:
[paste text]
```

### Profile and suppression

```
Check this UI copy with the relaxed flag:
[paste text]

Lint this document but ignore "軟件" for this run, explain all other issues:
[paste text]
```
