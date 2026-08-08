# CLI usage

## Linting files

```bash
# Single file
zhtw-mcp lint README.md

# Multiple files and directories (recursive)
zhtw-mcp lint docs/ src/locales/ README.md

# Stdin
zhtw-mcp lint -- < input.txt

# With options
zhtw-mcp lint file.md --format json --profile strict --max-errors 5
zhtw-mcp lint file.md --telemetry           # print stderr summary counters
zhtw-mcp lint file.md --format tabular              # aligned columns
zhtw-mcp lint docs/ --exclude "vendor/**"
zhtw-mcp lint -- --content-type markdown < input.md
zhtw-mcp lint -- --content-type markdown-scan-code < input.md  # also lint inside code blocks
```

## Auto-fix

```bash
zhtw-mcp lint file.md --fix                        # lexical_safe (default)
zhtw-mcp lint file.md --fix=orthographic           # punctuation/spacing/case/variant/grammar only
zhtw-mcp lint file.md --fix=lexical_contextual     # context-clue-gated rules too
zhtw-mcp lint file.md --fix --dry-run       # preview without writing
```

## Explaining flagged terms

```bash
zhtw-mcp lint file.md --explain
```

Each issue includes a cultural/linguistic annotation and its English anchor term.

## Scan caching

In lint-only mode (no `--fix`), the CLI automatically caches scan results keyed by file content hash (BLAKE3) and scan parameters. Unchanged files are skipped on subsequent runs. The cache lives at the platform default cache directory (`~/.cache/zhtw-mcp/` on Linux, `~/Library/Caches/zhtw-mcp/` on macOS) with 24-hour TTL and a 2000-entry cap. Caching is disabled when `--fix`, `--verify`, or stdin mode is active.

## Telemetry

Use `--telemetry` with `lint` to print a compact stderr summary after the run:

```bash
zhtw-mcp lint docs/ --telemetry
```

This reports processed file count plus total error/warning counts. It does not change stdout formatting or exit-code behavior.

## Judgment cache

LLM-backed disambiguation decisions are also persisted in a separate judgment cache. To clear it:

```bash
zhtw-mcp cache clear
```

## Output formats

| Format | Flag | Description |
|--------|------|-------------|
| `human` | _(default)_ | Colored, multi-line output for terminals |
| `json` | `--format json` | Machine-readable JSON array |
| `compact` | `--format compact` | One line per issue |
| `tabular` | `--format tabular` | Aligned columns for quick scanning |
| `sarif` | `--format sarif` | SARIF v2.1.0 for GitHub Code Scanning |

## CI/CD integration

```bash
# SARIF output for GitHub Code Scanning
zhtw-mcp lint docs/ --format sarif > results.sarif

# Baseline mode: suppress known issues, fail only on new ones
zhtw-mcp lint docs/ --baseline baseline.json

# Lint only files changed since a branch
zhtw-mcp lint --diff-from main
```

## Project config file

Create `.zhtw-mcp.toml` at your project root for team-wide settings:

```toml
profile = "strict"
max_errors = 0
max_warnings = 10
exclude = ["vendor/**", "*.bak"]
packs = ["medical"]
```

Discovered by walking from cwd upward to the `.git` root. CLI flags override config values. Supported fields: `profile`, `content_type`, `max_errors`, `max_warnings`, `ignore_terms`, `exclude`, `overrides`, `suppressions`, `packs`.

## Converting Simplified Chinese to Traditional

The `convert` subcommand converts Simplified Chinese (zh-CN) text to Traditional Chinese (zh-TW) and then applies the full lint/fix pipeline to normalize vocabulary:

```bash
# Convert a file (writes corrected output to stdout)
zhtw-mcp convert file.md

# Convert from stdin
zhtw-mcp convert -- < input.txt

# Specify content type explicitly
zhtw-mcp convert file.md --content-type markdown
```

This is a two-stage pipeline: first a built-in character/phrase converter (SC→TC), then iterative vocabulary normalization via the standard scanner.

When the `translate` feature is enabled, the `lint` subcommand supports `--verify` to confirm ambiguous substitutions against English anchor terms. The `convert` subcommand does not accept `--verify`; it runs the full calibration step unconditionally when the feature is active.

## Editor integration setup

Generate configuration snippets for MCP-capable editors:

```bash
zhtw-mcp setup claude-code
zhtw-mcp setup codex
```

Prints JSON configuration for the specified host. Available hosts depend on the build.

## Pre-commit hook

Add to your `.pre-commit-config.yaml`:

```yaml
- repo: https://github.com/<org>/zhtw-mcp
  hooks:
    - id: zhtw-mcp
```

The hook runs `zhtw-mcp lint` on staged Markdown, YAML, and text files.

## Rule packs

Domain-specific rule overlays stored as JSON files in the `packs/` subdirectory. Same schema as `overrides.json`. Layered on top of the base ruleset in `--pack` flag order.

```bash
zhtw-mcp pack import medical.json   # install a pack
zhtw-mcp pack export medical         # export a pack to medical.json
zhtw-mcp pack validate medical.json  # validate schema and check for issues
zhtw-mcp pack list                   # list installed packs
zhtw-mcp --pack medical lint file.md # activate pack for a lint run
zhtw-mcp --pack medical --pack legal # multiple packs
```
