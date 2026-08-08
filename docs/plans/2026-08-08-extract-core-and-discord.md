# zhtw Core and Discord Bot Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Preserve the upstream ruleset and engine, remove MCP as a runtime dependency, and expose a reusable zh-TW conversion core that can support a Discord bot and bounded Gemini context decisions.

**Architecture:** The existing scanner, ruleset, Tier 1/Tier 2 disambiguation, fixer, and validator remain the source of truth and are moved or re-exported behind a core conversion API. MCP transport, sampling, schemas, and MCP-only tests/docs are removed. Gemini receives only ambiguous issue context or an explicit rewrite request; every model result is validated by the core before it can be returned.

**Tech Stack:** Rust 2021, existing upstream dependencies and tests, serde/serde_json for stable LLM contracts, existing `ureq` feature for Gemini HTTP integration where needed.

---

### Task 1: Create the standalone repository baseline

**Files:**
- Existing source copied from upstream `sysprog21/zhtw-mcp` at the imported commit.
- Create: `docs/plans/2026-08-08-extract-core-and-discord.md`

**Step 1: Verify the source snapshot**

Run: `git diff --no-index /Users/takala/code/zhtw-mcp /Users/takala/code/zhtw-discord-bot` (excluding `.git` if needed)

Expected: no source-content differences before the planned changes.

**Step 2: Record the imported baseline**

Run: `git status --short --branch`

Expected: the new repository is on `codex/extract-zhtw-core` with the upstream import commit as its parent.

### Task 2: Extract the reusable core boundary without rewriting engine code

**Files:**
- Create: `src/core.rs`
- Modify: `src/lib.rs`
- Modify: `src/fixer.rs`
- Modify: `src/rules/ruleset.rs`
- Test: `tests/core-conversion.rs`

**Step 1: Write the core API test**

Cover deterministic correction, Tier 2 ambiguity exposure, externally selected candidate application, and post-fix validation using the embedded ruleset.

**Step 2: Run the focused test**

Run: `cargo test --test core-conversion`

Expected: initially fails because `core` and the decision API do not exist.

**Step 3: Implement the smallest core facade**

Reuse the existing `Scanner`, `disambiguate_batch`, `apply_fixes_with_context`, ruleset loader, S2T converter, and issue types. Do not duplicate rule data or reimplement scanning. Add validated external decisions for ambiguous issues and a result type containing original text, corrected text, issues, and validation status.

**Step 4: Run the focused test**

Run: `cargo test --test core-conversion`

Expected: PASS.

### Task 3: Remove MCP-only runtime and documentation

**Files:**
- Delete: `src/mcp/`
- Modify: `src/lib.rs`
- Modify: `src/main.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Delete: `tests/e2e-mcp.rs`
- Delete: `docs/mcp.md`
- Delete: `scripts/test-mcp-qwen.py`
- Modify: `README.md`

**Step 1: Remove MCP entry points**

Delete the MCP module, stdio server startup, MCP setup commands, MCP-specific test, MCP documentation, and MCP-only dependencies while keeping the copied CLI and engine behavior.

**Step 2: Run source-reference checks**

Run: `rg -n "MCP|mcp|sampling|zhtw-core" src tests docs README.md Cargo.toml`

Expected: no runtime MCP references; remaining historical/source attribution references are either removed or explicitly documented as upstream provenance.

**Step 3: Run the available static checks**

Run: `python3 scripts/check-ruleset.py --lint`

Expected: the copied ruleset count and duplicate check remain unchanged.

### Task 4: Add bounded Gemini contracts and validation

**Files:**
- Create: `src/llm.rs`
- Create: `src/gemini.rs`
- Modify: `src/lib.rs`
- Modify: `Cargo.toml`
- Test: `tests/llm-contract.rs`

**Step 1: Write contract tests**

Test that a context decision can only select an existing candidate and that malformed or out-of-set responses are rejected. Test rewrite validation preserves protected spans and rejects residual high-confidence issues.

**Step 2: Implement the contracts**

Define serializable request/response types for context decisions and explicit rewrites. Keep user text in data fields, not system instructions. Use the existing issue candidate list as the allowlist.

**Step 3: Implement the Gemini adapter**

Use the existing HTTP dependency only for the optional native Gemini feature. Keep API-key handling outside the core types and return structured errors on timeout, malformed JSON, or provider failure.

**Step 4: Run the focused tests**

Run: `cargo test --test llm-contract`

Expected: PASS.

### Task 5: Add the Discord adapter boundary

**Files:**
- Create: `apps/discord-bot/README.md`
- Create: `apps/discord-bot/config.example.env`
- Create: `src/discord_policy.rs`
- Create: `src/bin/discord-bot.rs`
- Modify: workspace configuration as required by the selected Discord client dependency.

**Step 1: Define bot policy tests**

Cover bot-message suppression, no-op replies, automatic bounded corrections, and explicit rewrite requests.

**Step 2: Implement the adapter**

Keep Discord event handling, privileged message-content configuration, rate limits, and reply formatting outside `zhtw-core`. Automatic replies use candidate decisions only; full rewriting requires an explicit command.

**Step 3: Run focused bot checks**

Run the bot package tests and a compile check.

Expected: PASS when the selected Discord client dependency is available.

### Task 6: Re-run the copied regression suite and commit

**Files:**
- Any files changed by Tasks 2–5.

**Step 1: Run ruleset and extension checks**

Run: `python3 scripts/check-ruleset.py --lint` and `npm test --prefix extension`

Expected: PASS with the upstream rule count unchanged and all extension tests passing.

**Step 2: Run Rust verification**

Run: `cargo test --lib` and the relevant integration tests.

Expected: PASS; if the environment lacks Cargo, report that limitation without claiming Rust verification.

**Step 3: Inspect the final diff**

Run: `git diff --stat` and `git diff --check`

Expected: no whitespace errors and no copied ruleset rewrite.

**Step 4: Commit the completed implementation**

Run: `git add -A && git commit -m "feat: extract zh-tw core for discord bot"`

Expected: a clean committed branch ready to publish as the new repository.
