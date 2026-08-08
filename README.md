# TWLinter

[![CI](https://github.com/TakalaWang/twlinter/actions/workflows/main.yml/badge.svg)](https://github.com/TakalaWang/twlinter/actions/workflows/main.yml)
[![Extension CI](https://github.com/TakalaWang/twlinter/actions/workflows/extension-ci.yml/badge.svg)](https://github.com/TakalaWang/twlinter/actions/workflows/extension-ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Traditional Chinese (zh-TW) linter, terminology converter, and Discord bot for
Taiwan-localized wording.

TWLinter detects Mainland Chinese terminology, punctuation, character variants,
translationese, and context-sensitive terms. It can report issues, apply safe
deterministic fixes, expose a reusable Rust core, and optionally use Gemini for
bounded contextual decisions or explicit rewrites.

## Upstream project and provenance

TWLinter is based on the original [sysprog21/zhtw-mcp](https://github.com/sysprog21/zhtw-mcp)
project. The upstream ruleset, scanner, S2T conversion data, Tier 1/Tier 2
logic, fixer, tests, and MIT license are preserved wherever possible rather than
rewritten from scratch.

This repository changes the integration boundary:

- removes MCP transport and MCP-only runtime code;
- exposes the ruleset and tier logic through `twlinter::core::CoreEngine`;
- keeps Gemini optional and restricts its choices to ruleset-provided candidates;
- adds a native Discord adapter for automatic corrections and `/tw-rewrite`.

Please see the upstream repository for the original project history and rule
provenance. Changes to copied rules should be made with that source relationship
in mind.

## Features

- Mainland terminology: 軟件→軟體、內存→記憶體、默認→預設
- Taiwan punctuation, quotation marks, spacing, casing, and grammar checks
- MoE standard character variants
- Built-in Simplified-to-Traditional conversion
- Context-sensitive terms such as 程序、進程、位操作, and 渲染
- Markdown, YAML, code-block, URL, path, and mention exclusions
- Profiles for base and strict checking
- Safe lexical fixes with post-fix validation
- Chrome extension for visible page text
- Optional Gemini contextual decisions and explicit prose rewriting
- Discord bot with protected URL, mention, and code spans

## Architecture

```text
CLI / Chrome Extension / Discord Bot
                │
                ▼
       twlinter::core::CoreEngine
                │
       ┌────────┼────────┐
       ▼        ▼        ▼
   ruleset    Tier 1    Tier 2
              scan      context
                │
                ▼
       bounded Gemini decisions
                │
                ▼
        fixer + post-scan validation
```

The core is transport-independent. External model output is accepted only when
it refers to an exact issue and selects one of that issue's existing ruleset
candidates. Full LLM rewriting is available only through the explicit
`/tw-rewrite` command in the Discord adapter.

## Quick start

Requires stable Rust 1.91 or newer.

```bash
git clone https://github.com/TakalaWang/twlinter.git
cd twlinter

make
make check
```

The standalone CLI is `target/release/twlinter`.

## CLI

```bash
twlinter lint README.md
twlinter lint file.md --fix
twlinter lint file.md --fix --dry-run
twlinter convert file.md
```

See [docs/cli.md](docs/cli.md) for formats, profiles, configuration, packs,
baselines, and CI integration.

## Rust core

```rust
use twlinter::core::{CoreEngine, CoreOptions};

let engine = CoreEngine::from_embedded(CoreOptions::default())?;
let analysis = engine.analyze("這個軟件會把數據存到內存。");
let result = engine.apply(&analysis, &[])?;
assert_eq!(result.text, "這個軟體會把資料存到記憶體。");
```

The core API is the preferred integration point for applications that do not
need the CLI or Discord adapter.

## Discord bot

The bot listens to Discord Gateway messages and requires the privileged
`MESSAGE_CONTENT` intent. Enable it in the Discord Developer Portal before
starting the bot.

```bash
export DISCORD_TOKEN="..."
export GEMINI_API_KEY="..."       # optional
export GEMINI_MODEL="gemini-2.5-flash"

cargo run --release --features discord --bin twlinter-discord
```

Without `GEMINI_API_KEY`, deterministic and locally resolved corrections still
work; unresolved ambiguous terms remain unchanged. Full rewriting requires:

```text
/tw-rewrite 原始訊息
```

See [apps/discord-bot/README.md](apps/discord-bot/README.md) for deployment
notes and [apps/discord-bot/config.example.env](apps/discord-bot/config.example.env)
for the environment template.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --features discord -- -D warnings
cargo test --all-targets --features discord
python3 scripts/check-ruleset.py --lint
npm test --prefix extension
```

The S2T tables are generated from OpenCC dictionaries by
`scripts/gen-s2t-tables.py` during builds. Do not hand-edit generated data.

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.

## Project documentation

- [CLI reference](docs/cli.md)
- [Processing pipeline and testing](docs/internals.md)
- [Rules and ruleset extensions](docs/rules.md)
- [Discord adapter](apps/discord-bot/README.md)
- [Contribution guide](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Changelog](CHANGELOG.md)

## License

TWLinter is released under the [MIT License](LICENSE).

The project is derived from [sysprog21/zhtw-mcp](https://github.com/sysprog21/zhtw-mcp);
please retain that attribution when redistributing the copied ruleset and engine.
