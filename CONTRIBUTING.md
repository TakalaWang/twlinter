# Contributing to TWLinter

Thank you for helping improve Taiwan-localized Traditional Chinese tooling.

## Before you start

- Read the [README](README.md) and [processing pipeline](docs/internals.md).
- Check existing issues and discussions before opening a new one.
- For security issues, follow [SECURITY.md](SECURITY.md) instead of opening a
  public issue.

## Local setup

```bash
git clone https://github.com/TakalaWang/twlinter.git
cd twlinter
rustup show
make check
```

The project uses stable Rust 1.91 or newer. The Discord bot additionally needs
`DISCORD_TOKEN` only for a live run; tests do not require credentials.

## Validation

Run the checks relevant to your change:

```bash
cargo fmt --check
cargo clippy --all-targets --features discord -- -D warnings
cargo test --all-targets --features discord
python3 scripts/check-ruleset.py --lint
npm test --prefix extension
```

If you change rules, run the ruleset lint and include the affected terminology,
context, and false-positive tests. If you change the extension, run its Node.js
test suite. If you change the Discord adapter, do not commit tokens or real
message content.

## Ruleset and upstream changes

TWLinter is derived from [sysprog21/zhtw-mcp](https://github.com/sysprog21/zhtw-mcp).
Keep copied engine behavior and ruleset provenance clear in the pull request.
Prefer reusing the existing scanner, Tier 2 logic, fixer, and validator over
duplicating them in an adapter.

## Pull requests

Please keep pull requests focused and describe:

1. what changed and why;
2. which user-facing behavior is affected;
3. which commands were run;
4. any upstream or ruleset attribution involved.

Small, reviewable commits are preferred. Do not include generated build output,
secrets, or unrelated formatting changes.
