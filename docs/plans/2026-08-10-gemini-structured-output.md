# Gemini Structured Output Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make Gemini context and rewrite responses schema-constrained and distinguish non-JSON text from JSON that fails contract deserialization.

**Architecture:** Keep the existing single Gemini adapter and pass a small response schema per contract. Extract the first non-thought, non-empty text part before parsing, because thinking models may return multiple parts.

**Tech Stack:** Rust, `serde_json`, Gemini `generateContent` REST API, Cargo tests.

---

### Task 1: Add contract schemas and response parsing checks

**Files:**
- Modify: `src/gemini.rs`

**Steps:**

1. Add JSON Schemas for `ContextResponse` and `RewriteResponse`.
2. Include each schema with `responseMimeType: application/json` in `generationConfig`.
3. Select the first non-thought, non-empty response text part.
4. Parse JSON syntax separately from contract deserialization.
5. Add unit tests for schema presence and thought-part selection.

### Task 2: Verify and deploy

**Files:**
- No additional source files.

**Steps:**

1. Run formatting, tests, Clippy, and release build.
2. Deploy the verified release binary to the existing launchd service.
3. Verify process state, binary hash, and latest connection/error log.
