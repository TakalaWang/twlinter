# Discord Webhook Replacement Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Restore the merged Discord correction flow that replaces eligible plain-text messages through a temporary webhook while keeping reply messages as threaded fallback replies.

**Architecture:** Reuse Serenity 0.12.5 webhook builders. The handler produces a bounded raw replacement, sends it with the original member display name and avatar, deletes the source only after the replacement succeeds, and falls back to the existing prefixed reply when identity or message metadata is unsafe or an API operation fails. Messages with reply references remain eligible for linting but are not webhook-replaced so their thread relationship is preserved.

**Tech Stack:** Rust, Serenity 0.12.5, Tokio, existing Discord policy helpers, GitHub Actions, launchd over Tailscale.

---

### Task 1: Restore bounded replacement policy

**Files:**
- Modify: `src/discord_policy.rs`
- Test: `tests/discord-policy.rs`

Add raw replacement helpers that reject empty or over-limit content while keeping the existing prefixed reply helpers for fallback. Test both bounded replacement and fallback formatting.

### Task 2: Integrate safe webhook replacement

**Files:**
- Modify: `src/bin/discord-bot.rs`

Route deterministic and validated Gemini corrections through webhook replacement when the message is plain text with guild member identity and no unsupported metadata. Send before deleting, clean up on deletion failure, and use the existing reply fallback for replies and failed operations. Add focused tests for the safety boundary where practical.

### Task 3: Document runtime permissions

**Files:**
- Modify: `apps/discord-bot/README.md`

Document the `MANAGE_MESSAGES` and `MANAGE_WEBHOOKS` requirements and the fallback boundary.

### Task 4: Verify, merge, and deploy

Run the repository checks and release build on the PR branch, create a ready PR, recheck its current head and required CI, squash-merge it, build from the merge SHA, and deploy the resulting binary using the verified remote SHA/backup/launchd procedure. Confirm the service, process, binary hash, and fresh Discord connection log.
