# CLI usage

## Linting files

```bash
# Single file
twlinter lint README.md

# Multiple files and directories (recursive)
twlinter lint docs/ src/locales/ README.md

# Stdin
twlinter lint -- < input.txt

# With options
twlinter lint file.md --format json --profile strict --max-errors 5
twlinter lint file.md --telemetry           # print stderr summary counters
twlinter lint file.md --format tabular              # aligned columns
twlinter lint docs/ --exclude "vendor/**"
twlinter lint -- --content-type markdown < input.md
twlinter lint -- --content-type markdown-scan-code < input.md  # also lint inside code blocks
```

## Auto-fix

```bash
twlinter lint file.md --fix                        # lexical_safe (default)
twlinter lint file.md --fix=orthographic           # punctuation/spacing/case/variant/grammar only
twlinter lint file.md --fix=lexical_contextual     # context-clue-gated rules too
twlinter lint file.md --fix --dry-run       # preview without writing
```

## Explaining flagged terms

```bash
twlinter lint file.md --explain
```

Each issue includes a cultural/linguistic annotation and its English anchor term.

## Scan caching

In lint-only mode (no `--fix`), the CLI automatically caches scan results keyed by file content hash (BLAKE3) and scan parameters. Unchanged files are skipped on subsequent runs. The cache lives at the platform default cache directory (`~/.cache/twlinter/` on Linux, `~/Library/Caches/twlinter/` on macOS) with 24-hour TTL and a 2000-entry cap. Caching is disabled when `--fix`, `--verify`, or stdin mode is active.

## Telemetry

Use `--telemetry` with `lint` to print a compact stderr summary after the run:

```bash
twlinter lint docs/ --telemetry
```

This reports processed file count plus total error/warning counts. It does not change stdout formatting or exit-code behavior.

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
twlinter lint docs/ --format sarif > results.sarif

# Baseline mode: suppress known issues, fail only on new ones
twlinter lint docs/ --baseline baseline.json

# Lint only files changed since a branch
twlinter lint --diff-from main
```

## Project config file

Create `.twlinter.toml` at your project root for team-wide settings:

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
twlinter convert file.md

# Convert from stdin
twlinter convert -- < input.txt

# Specify content type explicitly
twlinter convert file.md --content-type markdown
```

This is a two-stage pipeline: first a built-in character/phrase converter (SC→TC), then iterative vocabulary normalization via the standard scanner.

When the `translate` feature is enabled, the `lint` subcommand supports `--verify` to confirm ambiguous substitutions against English anchor terms. The `convert` subcommand does not accept `--verify`; it runs the full calibration step unconditionally when the feature is active.

## Pre-commit hook

Add to your `.pre-commit-config.yaml`:

```yaml
- repo: https://github.com/<org>/twlinter
  hooks:
    - id: twlinter
```

The hook runs `twlinter lint` on staged Markdown, YAML, and text files.

## Rule packs

Domain-specific rule overlays stored as JSON files in the `packs/` subdirectory. Same schema as `overrides.json`. Layered on top of the base ruleset in `--pack` flag order.

```bash
twlinter pack import medical.json   # install a pack
twlinter pack export medical         # export a pack to medical.json
twlinter pack validate medical.json  # validate schema and check for issues
twlinter pack list                   # list installed packs
twlinter --pack medical lint file.md # activate pack for a lint run
twlinter --pack medical --pack legal # multiple packs
```
