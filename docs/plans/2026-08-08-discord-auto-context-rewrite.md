# Discord 自動情境改寫 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make Gemini the default context rewrite path for enabled Discord channels, while retaining slash commands only for channel registration.

**Architecture:** Add a small JSON-backed channel allowlist shared by the Discord message and interaction handlers. Keep `CoreEngine`, `RewriteRequest`, protected-span validation, and post-rewrite analysis as the safety boundary; the message handler uses Gemini automatically and falls back to deterministic output on any provider or validation failure.

**Tech Stack:** Rust 2021, Serenity 0.12, existing serde/serde_json, Tokio, current TWLinter core and Gemini adapter.

---

### Task 1: Record and verify the design baseline

**Files:**
- Create: `docs/plans/2026-08-08-discord-auto-context-rewrite-design.md`
- Create: `docs/plans/2026-08-08-discord-auto-context-rewrite.md`

**Step 1: Verify the existing adapter flow**

Run: `sed -n '1,150p' src/bin/discord-bot.rs`

Expected: current behavior has an explicit `/tw-rewrite` branch and deterministic automatic replies.

**Step 2: Commit the design documents**

Run: `git add docs/plans/2026-08-08-discord-auto-context-rewrite*.md && git commit -m "docs: design automatic discord context rewrite"`

Expected: the isolated branch records the accepted architecture before source changes.

### Task 2: Add the persisted channel allowlist

**Files:**
- Create: `src/discord_channels.rs`
- Modify: `src/lib.rs`
- Test: unit tests in `src/discord_channels.rs`

**Step 1: Write registry tests**

Cover an empty path, `enable`, `disable`, `is_enabled`, `list`, and loading the persisted JSON into a new registry.

**Step 2: Run the focused test**

Run: `cargo test --all-features discord_channels`

Expected: the new module or methods are initially absent/failing.

**Step 3: Implement the minimal registry**

Store sorted `u64` channel IDs in a JSON file selected by `TWLINTER_CHANNELS_FILE`, defaulting to `twlinter-channels.json`. Protect the in-memory set with a standard mutex and write only after a successful set mutation.

**Step 4: Run the focused test**

Run: `cargo test --all-features discord_channels`

Expected: all registry tests pass.

### Task 3: Change message handling to automatic Gemini rewrite

**Files:**
- Modify: `src/bin/discord-bot.rs`
- Modify: `src/discord_policy.rs`
- Test: existing policy tests plus a new handler-policy unit test if needed

**Step 1: Remove the user rewrite trigger**

Delete `REWRITE_COMMAND` and stop branching on message prefixes. Every non-empty message in an enabled channel follows the same pipeline.

**Step 2: Gate ordinary messages by the registry**

Ignore ordinary messages until their `channel_id` is registered. Keep Bot-message suppression before registry and engine work.

**Step 3: Call Gemini automatically for changed drafts**

After deterministic analysis/application, call the existing Gemini rewrite contract whenever a changed draft exists and a Gemini client is configured. Accept only a safe response whose re-analysis has no remaining issues; otherwise use the deterministic reply.

**Step 4: Run focused tests and compile**

Run: `cargo test --all-features discord_policy` and `cargo build --release --features discord --bin twlinter-discord`

Expected: policy tests pass and the Discord binary compiles.

### Task 4: Add management-only channel commands

**Files:**
- Modify: `src/bin/discord-bot.rs`
- Modify: `apps/discord-bot/README.md`
- Modify: `apps/discord-bot/config.example.env`
- Modify: `README.md`

**Step 1: Register a local guild command**

Create `/twlinter` with `enable`, `disable`, and `status` subcommands on each guild at `ready`. Set default member permission to `MANAGE_GUILD` and keep the handler permission check.

**Step 2: Handle interactions**

Enable or disable the current channel, report the configured channels, and use ephemeral responses. Do not accept message text or rewrite options in the command.

**Step 3: Document the operator flow**

Document `TWLINTER_CHANNELS_FILE`, the three management subcommands, and the fact that ordinary messages are rewritten automatically after channel enablement.

**Step 4: Run the full verification suite**

Run: `cargo fmt --check`; `cargo clippy --all-targets --features discord -- -D warnings`; `cargo test --all-features`; `cargo build --release --features discord --bin twlinter-discord`; `python3 scripts/check-ruleset.py --lint`; `git diff --check`.

Expected: all commands exit 0 with no formatting, lint, test, build, or whitespace errors.

### Task 5: Commit and hand off deployment

**Files:**
- All files changed by Tasks 2–4.

**Step 1: Inspect scope**

Run: `git status --short` and `git diff --stat`.

Expected: only the automatic rewrite, channel registry, docs, and tests are changed.

**Step 2: Commit implementation**

Run: `git add src apps README.md docs/plans && git commit -m "feat: enable automatic discord context rewrites"`

Expected: a committed isolated branch ready to build and deploy after `DISCORD_TOKEN` is supplied on the server.
