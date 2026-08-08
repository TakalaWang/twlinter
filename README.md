# TWLinter

A Traditional Chinese (zh-TW) linter and conversion core for correcting
Mainland Chinese terminology, punctuation, character variants, and common
translationese before text reaches users.

The ruleset is copied from the upstream zhtw-mcp project and remains the source
of truth. This repository removes MCP as a runtime dependency and exposes the
same scanner, rules, Tier 1/Tier 2 disambiguation, fixer, and validator through
the reusable twlinter::core::CoreEngine API.

## What it checks

- Mainland vocabulary: 軟件→軟體、內存→記憶體、默認→預設
- Taiwan punctuation and quotation marks
- MoE standard character variants
- CJK and Latin/digit spacing
- Casing and selected grammar patterns
- Context-sensitive terms such as 程序、進程、位操作 and 渲染

Profiles are preserved from upstream:

| Profile | Purpose |
|---------|---------|
| base | Cross-strait vocabulary, punctuation, casing, grammar, and political terms |
| strict | Base rules plus character variants and full MoE enforcement |

## Build and test

Requires stable Rust 1.91 or newer.

~~~
make
make check
~~~

The standalone binary is target/release/twlinter.

## CLI

~~~
twlinter lint README.md
twlinter lint file.md --fix
twlinter lint file.md --fix --dry-run
twlinter convert file.md
~~~

## Discord bot

The bot uses Discord Gateway events and enables the MESSAGE_CONTENT
privileged intent because it must inspect normal message text. Enable that
intent in the Discord Developer Portal before starting the bot.

~~~
export DISCORD_TOKEN="..."
export GEMINI_API_KEY="..."
export GEMINI_MODEL="gemini-2.5-flash"
cargo run --features discord --bin twlinter-discord
~~~

Without GEMINI_API_KEY, deterministic and locally resolved corrections still
work. Ambiguous context decisions are left unchanged. Automatic replies only
return when the message changes. Full rewriting requires the explicit command:

~~~
/tw-rewrite 原始訊息
~~~

Gemini receives only the original text, relevant issue candidates, context
clues, and protected spans. Its output is validated against the ruleset and
scanned again before the bot replies.

## Architecture

~~~
Discord Bot ──┐
              ├── twlinter::core::CoreEngine
CLI ──────────┘             │
                            ├── ruleset
                            ├── Tier 1 deterministic scan
                            ├── Tier 2 local context scoring
                            ├── bounded Gemini decisions
                            └── post-fix validation
~~~

MCP is not part of this repository. If another application needs the core, it
should call the library or its CLI instead of coupling the rules engine to a
transport protocol.

## Further reading

- docs/cli.md — CLI reference and configuration
- docs/internals.md — processing pipeline and testing
- docs/rules.md — rule reference and ruleset extension
- docs/plans/2026-08-08-extract-core-and-discord.md — architecture plan

## License

TWLinter is available under the MIT license. See LICENSE.
