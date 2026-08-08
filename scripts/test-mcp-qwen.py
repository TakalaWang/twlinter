#!/usr/bin/env python3
"""Automated MCP server validation via Qwen Code CLI.

Sends natural-language prompts through `qwen -y -p` that exercise the
zhtw-mcp MCP tools, then checks stdout for expected keywords to confirm
correct zh-TW output.

Tools exercised:
  - zhtw (lint, fix, gate, explain, compact, political_stance, lexical_contextual)

Usage:
    python3 scripts/test-mcp-qwen.py            # run all tests
    python3 scripts/test-mcp-qwen.py -v          # verbose (show full output)
    python3 scripts/test-mcp-qwen.py -k lint     # run only tests matching "lint"
    python3 scripts/test-mcp-qwen.py --timeout 90 --retries 0  # fail-fast
    python3 scripts/test-mcp-qwen.py --diagnose  # extended pre-flight diagnostics
    python3 scripts/test-mcp-qwen.py --health    # MCP server health check only
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from enum import Enum

PROJECT_ROOT = pathlib.Path(__file__).resolve().parent.parent

# ---------------------------------------------------------------------------
# Result classification
# ---------------------------------------------------------------------------


class Status(Enum):
    PASS = "PASS"
    FAIL = "FAIL"
    TIMEOUT = "TIMEOUT"
    CRASH = "CRASH"
    SKIP = "SKIP"


# ---------------------------------------------------------------------------
# Test definitions
# ---------------------------------------------------------------------------


@dataclass
class TestCase:
    name: str
    prompt: str
    expect_any: list[str] = field(default_factory=list)
    expect_all: list[str] = field(default_factory=list)
    reject_any: list[str] = field(default_factory=list)
    timeout: int | None = None  # per-test override; None = use global default


TESTS: list[TestCase] = [
    # ---- zhtw: lint only (fix_mode absent -> none) ----
    TestCase(
        name="lint_basic",
        prompt=(
            "使用 zhtw 工具檢查以下文字（不要設 fix_mode），"
            "只列出問題和建議修正，用表格格式回答：\n"
            "「軟體工程師需要優化數據庫的性能，通過調試程序來排查代碼中的問題。」"
        ),
        expect_all=["資料庫", "效能", "除錯", "程式碼"],
        expect_any=["數據庫", "性能", "調試", "代碼"],
        timeout=60,
    ),
    # ---- zhtw: strict_moe profile ----
    TestCase(
        name="lint_strict_moe",
        prompt=(
            "使用 zhtw 工具，profile 設為 strict_moe，"
            "檢查：「應用程式需要讀取數據並進行網絡通訊。」\n"
            "只列出問題和建議。"
        ),
        expect_any=["資料", "數據", "網路", "網絡"],
        timeout=60,
    ),
    # ---- zhtw: fix_mode lexical_safe ----
    TestCase(
        name="fix_safe",
        prompt=(
            "使用 zhtw 工具，fix_mode 設為 lexical_safe，修正以下文字：\n"
            "「開發人員利用調試工具來優化軟件的性能。」\n"
            "只回答修正後的完整文字。"
        ),
        expect_all=["除錯", "軟體", "效能"],
        timeout=60,
    ),
    # ---- zhtw: gate pass ----
    TestCase(
        name="gate_pass",
        prompt=(
            "使用 zhtw 工具，fix_mode=lexical_safe，max_errors=0，"
            "處理：「軟體工程師使用資料庫來開發應用程式。」\n"
            "只回答 accepted 值（true/false）。"
        ),
        expect_any=["true", "accept", "接受", "通過"],
        reject_any=["false", "reject", "拒絕"],
        timeout=60,
    ),
    # ---- zhtw: gate reject ----
    TestCase(
        name="gate_reject",
        prompt=(
            "使用 zhtw 工具，fix_mode=lexical_safe，max_errors=0，"
            "處理：「開發人員優化軟件性能並調試代碼。」\n"
            "只回答 accepted 值和剩餘 errors 數量。"
        ),
        expect_any=["false", "reject", "拒絕", "error", "錯誤"],
        timeout=60,
    ),
    # ---- zhtw: ignore_terms ----
    TestCase(
        name="ignore_terms",
        prompt=(
            '使用 zhtw 工具，ignore_terms=["軟件"]，'
            "檢查：「這個軟件很好用」\n"
            "只回答軟件這個詞的 severity 值。"
        ),
        expect_any=["info"],
        reject_any=["warning"],
        timeout=60,
    ),
    # ---- zhtw: markdown content_type ----
    TestCase(
        name="markdown_exclusion",
        prompt=(
            "使用 zhtw 工具，content_type=markdown，"
            "檢查：「這個軟件很好用\n\n```\n軟件代碼\n```\n\n軟件很棒」\n"
            "回答：code block 內容是否被排除？共幾個 issues？"
        ),
        expect_any=["排除", "exclude", "跳過", "skip", "2", "兩"],
        # No reject_any: the LLM legitimately mentions "3" when explaining that
        # 3 occurrences exist but only 2 are issues after code-block exclusion.
        timeout=120,
    ),
    # ---- zhtw: return shape ----
    TestCase(
        name="return_shape",
        prompt=(
            "使用 zhtw 工具，fix_mode=lexical_safe，max_errors=5，"
            "處理：「這個軟件用了很多內存」\n"
            "只列出回傳結果中 accepted、applied_fixes、summary、gate 的值。"
        ),
        expect_any=["accepted", "true"],
        expect_all=["applied_fixes", "summary", "gate"],
        timeout=60,
    ),
    # ---- zhtw: explain mode ----
    TestCase(
        name="explain_mode",
        prompt=(
            "使用 zhtw 工具，explain=true，"
            "檢查：「這個軟件的性能很差」\n"
            "列出每個 issue 的 explanation 欄位內容。"
        ),
        # build_explanation() produces English prose containing these terms
        expect_any=[
            "explanation",
            "mainland",
            "Taiwan",
            "term",
            "standard",
            "uses",
            "軟體",
            "效能",
        ],
        timeout=60,
    ),
    # ---- zhtw: political_stance neutral ----
    TestCase(
        name="political_neutral",
        prompt=(
            "使用 zhtw 工具，political_stance=neutral，"
            "檢查：「大陸的經濟發展很快」\n"
            "只回答 issues 數量。"
        ),
        expect_any=["0", "沒有", "no issue", "無"],
        timeout=60,
    ),
    # ---- zhtw: output compact ----
    TestCase(
        name="output_compact",
        prompt=(
            "使用 zhtw 工具，output=compact，"
            "檢查：「軟件性能優化和數據庫調試」\n"
            "只列出每個 issue 的 found 和 suggestions，不要解釋。"
        ),
        expect_any=["軟體", "效能", "資料庫", "除錯"],
        timeout=60,
    ),
    # ---- zhtw: fix_mode lexical_contextual ----
    TestCase(
        name="fix_lexical_contextual",
        prompt=(
            "使用 zhtw 工具，fix_mode=lexical_contextual，"
            "修正：「軟件工程師優化數據庫性能並調試代碼」\n"
            "只回答修正後文字。"
        ),
        # Server output: "軟體工程師最佳化資料庫效能並除錯程式碼"
        # LLM may paraphrase; require 2+ core terms, accept any of the full set.
        expect_all=["效能", "除錯"],
        expect_any=["軟體", "程式碼", "資料庫", "最佳化"],
        timeout=60,
    ),
]


API_ERROR_MARKERS = [
    "API Error",
    "inappropriate content",
    "rate limit",
    "quota exceeded",
    "quota has been exhausted",
    "429",
    "500",
    "502",
    "503",
    "unexpected critical error",
]

# ---------------------------------------------------------------------------
# Runner
# ---------------------------------------------------------------------------


def is_api_error(output: str) -> bool:
    """Detect Qwen API / content-filter errors in output."""
    return any(marker.lower() in output.lower() for marker in API_ERROR_MARKERS)


def _safe_decode(raw: bytes) -> str:
    """Decode subprocess output, tolerant of encoding issues."""
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        return raw.decode("utf-8", errors="replace")


def _kill_process_group(proc: subprocess.Popen) -> None:  # type: ignore[type-arg]
    """Send SIGTERM then SIGKILL to the entire process group.

    Requires the subprocess to have been launched with start_new_session=True
    so it has its own process group.  Falls back to proc.kill() if the pgid
    kill fails (e.g. process already reaped).
    """
    try:
        os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
    except (OSError, ProcessLookupError):
        pass
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except (OSError, ProcessLookupError):
            proc.kill()


def run_qwen(
    prompt: str, timeout: int, retries: int = 1, retry_delay: float = 5.0
) -> tuple[str, float, int | None]:
    """Run qwen -y -p <prompt>, retrying on API errors.

    Retries use exponential backoff (5s, 10s, 20s, ...) capped against
    a deadline budget so total wall-clock stays close to `timeout`.

    Returns (output, elapsed, returncode). returncode is None on timeout.
    """
    deadline = time.monotonic() + timeout
    last_output = ""
    last_rc: int | None = None
    overall_start = time.monotonic()

    for attempt in range(1 + retries):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            elapsed = time.monotonic() - overall_start
            return (
                f"[TIMEOUT after {elapsed:.0f}s across {attempt} attempts]",
                elapsed,
                None,
            )

        attempt_timeout = max(5, int(remaining))

        try:
            with subprocess.Popen(
                ["qwen", "-y", "-p", prompt],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,  # own process group for clean kill
            ) as proc:
                try:
                    stdout_bytes, stderr_bytes = proc.communicate(
                        timeout=attempt_timeout
                    )
                    last_output = _safe_decode(stdout_bytes) + _safe_decode(
                        stderr_bytes
                    )
                    last_rc = proc.returncode
                except subprocess.TimeoutExpired:
                    # Kill entire process group so qwen's children don't leak.
                    _kill_process_group(proc)
                    try:
                        proc.communicate(timeout=5)
                    except subprocess.TimeoutExpired:
                        pass
                    elapsed = time.monotonic() - overall_start
                    return f"[TIMEOUT after {elapsed:.0f}s]", elapsed, None
                except KeyboardInterrupt:
                    _kill_process_group(proc)
                    proc.communicate()
                    raise
        except FileNotFoundError:
            elapsed = time.monotonic() - overall_start
            return "[qwen binary not found]", elapsed, 127

        # Non-zero exit that isn't a timeout -- report immediately
        if last_rc != 0:
            elapsed = time.monotonic() - overall_start
            return last_output, elapsed, last_rc

        if is_api_error(last_output) and attempt < retries:
            # Exponential backoff: 5s, 10s, 20s, ... capped to remaining
            backoff = retry_delay * (2**attempt)
            sleep_time = min(backoff, max(0, deadline - time.monotonic()))
            if sleep_time > 0:
                time.sleep(sleep_time)
            continue

        elapsed = time.monotonic() - overall_start
        return last_output, elapsed, last_rc


def check_result(tc: TestCase, output: str) -> tuple[bool, list[str]]:
    """Check test expectations. Returns (passed, list_of_failure_reasons)."""
    if is_api_error(output):
        return False, ["API error (content filter or rate limit)"]

    failures: list[str] = []
    lower = output.lower()

    if tc.expect_all:
        for kw in tc.expect_all:
            if kw.lower() not in lower:
                failures.append(f"missing required keyword: {kw!r}")

    if tc.expect_any:
        if not any(kw.lower() in lower for kw in tc.expect_any):
            failures.append(f"none of these keywords found: {tc.expect_any!r}")

    if tc.reject_any:
        for kw in tc.reject_any:
            if kw.lower() in lower:
                failures.append(f"unwanted keyword present: {kw!r}")

    return len(failures) == 0, failures


def classify_result(
    tc: TestCase, output: str, rc: int | None
) -> tuple[Status, list[str]]:
    """Classify a test result into PASS/FAIL/TIMEOUT/CRASH/SKIP."""
    if rc is None:
        return Status.TIMEOUT, ["timed out"]

    if rc != 0:
        return Status.CRASH, [f"non-zero exit code: {rc}"]

    ok, reasons = check_result(tc, output)
    if ok:
        return Status.PASS, []

    is_api = any("API error" in r for r in reasons)
    if is_api:
        return Status.SKIP, reasons

    return Status.FAIL, reasons


# ---------------------------------------------------------------------------
# MCP server health check (direct JSON-RPC, no qwen dependency)
# ---------------------------------------------------------------------------


def _find_mcp_binary() -> pathlib.Path | None:
    """Locate zhtw-mcp binary (release preferred over debug)."""
    release = PROJECT_ROOT / "target" / "release" / "zhtw-mcp"
    debug = PROJECT_ROOT / "target" / "debug" / "zhtw-mcp"
    if release.exists():
        return release
    if debug.exists():
        return debug
    return None


def _jsonrpc_request(method: str, params: dict | None = None, req_id: int = 1) -> str:
    """Build a newline-terminated JSON-RPC 2.0 request."""
    msg: dict = {"jsonrpc": "2.0", "method": method, "id": req_id}
    if params is not None:
        msg["params"] = params
    return json.dumps(msg) + "\n"


def _jsonrpc_notification(method: str) -> str:
    """Build a newline-terminated JSON-RPC 2.0 notification (no id)."""
    return json.dumps({"jsonrpc": "2.0", "method": method}) + "\n"


@dataclass
class HealthResult:
    ok: bool
    server_name: str = ""
    protocol_version: str = ""
    tools: list[str] = field(default_factory=list)
    tool_check_ok: bool = False
    tool_check_detail: str = ""
    errors: list[str] = field(default_factory=list)
    elapsed: float = 0.0


def mcp_health_check(timeout: int = 15) -> HealthResult:
    """Spawn zhtw-mcp and verify JSON-RPC handshake + basic tool call.

    Tests the full protocol stack without requiring qwen:
    1. initialize handshake (protocol version, server capabilities)
    2. tools/list (verify all 5 tools registered)
    3. tools/call zhtw with a known-bad input (verify issue detection)
    4. graceful shutdown
    """
    result = HealthResult(ok=False)
    start = time.monotonic()

    binary = _find_mcp_binary()
    if binary is None:
        result.errors.append("zhtw-mcp binary not found (run: cargo build --release)")
        return result

    proc: subprocess.Popen[bytes] | None = None

    # Use TemporaryDirectory for robust cleanup
    with tempfile.TemporaryDirectory(prefix="zhtw-health-") as tmp:
        try:
            overrides_path = os.path.join(tmp, "overrides.json")
            suppressions_path = os.path.join(tmp, "suppressions.json")

            # Create empty config files to ensure server doesn't error on missing file
            with open(overrides_path, "w", encoding="utf-8") as f:
                f.write("{}")
            with open(suppressions_path, "w", encoding="utf-8") as f:
                f.write("{}")

            proc = subprocess.Popen(
                [
                    str(binary),
                    "--overrides",
                    overrides_path,
                    "--suppressions",
                    suppressions_path,
                ],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,  # discard; avoids pipe-buffer deadlock
            )
            assert proc.stdin is not None
            assert proc.stdout is not None

            def send(data: str) -> None:
                assert proc is not None and proc.stdin is not None
                try:
                    proc.stdin.write(data.encode("utf-8"))
                    proc.stdin.flush()
                except OSError:
                    # Process likely died; recv will catch EOF or timeout
                    pass

            # Persistent receive buffer across recv() calls so bytes
            # arriving after the first newline are not lost.
            import select as _select

            _recv_buf = bytearray()
            _poller = _select.poll()
            _poller.register(proc.stdout, _select.POLLIN)

            def recv(label: str) -> dict:
                assert proc is not None and proc.stdout is not None
                # Use poll(2) + os.read() instead of readline().
                # readline() can block indefinitely if the server writes a
                # partial line then hangs -- poll only guarantees *some*
                # data is readable, not a full line.  poll(2) is cheaper
                # than select(2): no fd_set rebuild per call, O(1) for a
                # single fd, and no FD_SETSIZE (1024) ceiling.
                recv_deadline = time.monotonic() + timeout
                while b"\n" not in _recv_buf:
                    remaining_ms = int((recv_deadline - time.monotonic()) * 1000)
                    if remaining_ms <= 0:
                        raise TimeoutError(
                            f"no complete response for {label} within {timeout}s"
                        )
                    events = _poller.poll(remaining_ms)
                    if not events:
                        raise TimeoutError(f"no response for {label} within {timeout}s")
                    chunk = os.read(proc.stdout.fileno(), 65536)
                    if not chunk:
                        raise RuntimeError(f"EOF while waiting for {label}")
                    _recv_buf.extend(chunk)
                # Extract first complete line; keep remainder for next call.
                idx = _recv_buf.index(b"\n") + 1
                line = bytes(_recv_buf[:idx])
                del _recv_buf[:idx]
                try:
                    return json.loads(line)
                except json.JSONDecodeError:
                    raise RuntimeError(
                        f"invalid JSON from server for {label}: {line[:200]!r}"
                    )

            # Step 1: initialize
            send(
                _jsonrpc_request(
                    "initialize",
                    {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": {"name": "health-check", "version": "0.1"},
                    },
                    req_id=1,
                )
            )

            resp = recv("initialize")
            if "error" in resp:
                result.errors.append(f"initialize error: {resp['error']}")
                return result

            init_result = resp.get("result", {})
            result.server_name = init_result.get("serverInfo", {}).get("name", "")
            result.protocol_version = init_result.get("protocolVersion", "")

            caps = init_result.get("capabilities", {})
            if "tools" not in caps:
                result.errors.append("server missing tools capability")
                return result

            # Step 2: notifications/initialized
            send(_jsonrpc_notification("notifications/initialized"))

            # Step 3: tools/list
            send(_jsonrpc_request("tools/list", {}, req_id=2))
            resp = recv("tools/list")
            if "error" in resp:
                result.errors.append(f"tools/list error: {resp['error']}")
                return result

            tools = resp.get("result", {}).get("tools", [])
            result.tools = [t.get("name", "") for t in tools]

            expected_tools = {
                "zhtw",
            }
            missing = expected_tools - set(result.tools)
            if missing:
                result.errors.append(f"missing tools: {sorted(missing)}")
                return result

            # Step 4: tools/call -- verify zhtw produces a valid issue
            send(
                _jsonrpc_request(
                    "tools/call",
                    {
                        "name": "zhtw",
                        "arguments": {"text": "這個軟件很好用"},
                    },
                    req_id=3,
                )
            )
            resp = recv("tools/call zhtw")
            if "error" in resp:
                result.errors.append(f"zhtw call error: {resp['error']}")
                return result

            content = resp.get("result", {}).get("content", [])
            if not content:
                result.errors.append("zhtw returned empty content")
                return result

            try:
                output = json.loads(content[0].get("text", "{}"))
            except json.JSONDecodeError as e:
                result.errors.append(f"zhtw output not valid JSON: {e}")
                return result

            issues = output.get("issues", [])
            found_software = any(i.get("found") == "軟件" for i in issues)
            if found_software:
                result.tool_check_ok = True
                result.tool_check_detail = "correctly flagged 軟件→軟體"
            else:
                result.tool_check_ok = False
                result.tool_check_detail = (
                    f"expected 軟件 issue, got {len(issues)} issue(s)"
                )
                # Not a fatal error -- tool responded, just unexpected output
                result.errors.append(f"zhtw validation: {result.tool_check_detail}")

            result.ok = len(result.errors) == 0

        except (TimeoutError, RuntimeError) as exc:
            result.errors.append(str(exc))
        except OSError as exc:
            result.errors.append(f"OS error: {exc}")
        except KeyboardInterrupt:
            if proc is not None:
                proc.kill()
            raise
        finally:
            result.elapsed = time.monotonic() - start
            if proc is not None:
                if proc.stdin is not None:
                    try:
                        proc.stdin.close()
                    except OSError:
                        pass
                if proc.stdout is not None:
                    try:
                        proc.stdout.close()
                    except OSError:
                        pass
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        pass
    return result


def print_health_report(hr: HealthResult) -> None:
    """Print a formatted MCP health check report."""
    print("MCP Server Health Check:")
    if hr.ok:
        print(f"  [OK] server: {hr.server_name}")
        print(f"  [OK] protocol: {hr.protocol_version}")
        print(f"  [OK] tools: {', '.join(hr.tools)} ({len(hr.tools)} registered)")
        tag = "OK" if hr.tool_check_ok else "WARN"
        print(f"  [{tag}] zhtw: {hr.tool_check_detail}")
        print(f"  [OK] healthy ({hr.elapsed:.1f}s)")
    else:
        for err in hr.errors:
            print(f"  [FAIL] {err}")
        if hr.server_name:
            print(f"  [INFO] server: {hr.server_name}")
        if hr.tools:
            print(f"  [INFO] tools: {', '.join(hr.tools)}")
        print(f"  [FAIL] unhealthy ({hr.elapsed:.1f}s)")
    print()


# ---------------------------------------------------------------------------
# Pre-flight checks
# ---------------------------------------------------------------------------


def check_qwen_binary() -> bool:
    """Verify qwen binary exists and responds."""
    try:
        r = subprocess.run(
            ["qwen", "--version"],
            capture_output=True,
            timeout=10,
        )
        return r.returncode == 0
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return False


def check_mcp_binary() -> tuple[bool, str]:
    """Check if zhtw-mcp binary exists and is fresh.

    Returns (exists, message).
    """
    binary = _find_mcp_binary()
    if binary is None:
        return False, "zhtw-mcp binary not found (run: cargo build --release)"

    # Check staleness: compare binary mtime against source and build files
    binary_mtime = binary.stat().st_mtime
    newest_src = 0.0
    for f in (PROJECT_ROOT / "src").rglob("*.rs"):
        newest_src = max(newest_src, f.stat().st_mtime)
    for name in ("Cargo.toml", "Cargo.lock"):
        p = PROJECT_ROOT / name
        if p.exists():
            newest_src = max(newest_src, p.stat().st_mtime)

    if newest_src > binary_mtime:
        return (
            True,
            f"WARNING: {binary.name} is stale (older than src/). Rebuild recommended.",
        )

    return True, f"{binary.name} found and up to date"


def smoke_test(timeout: int = 45) -> tuple[bool, str]:
    """Run a minimal zhtw invocation to verify MCP server connectivity."""
    prompt = "使用 zhtw 工具檢查「測試」，只回答 issue 數量"
    try:
        with subprocess.Popen(
            ["qwen", "-y", "-p", prompt],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        ) as proc:
            try:
                stdout_bytes, stderr_bytes = proc.communicate(timeout=timeout)
                output = _safe_decode(stdout_bytes) + _safe_decode(stderr_bytes)
                if proc.returncode != 0:
                    return (
                        False,
                        f"smoke test failed (rc={proc.returncode}): {output[:200]}",
                    )
                if is_api_error(output):
                    return False, f"smoke test hit API error: {output[:200]}"
                return True, "smoke test passed"
            except subprocess.TimeoutExpired as te:
                _kill_process_group(proc)
                partial = ""
                # First try the exception's captured output
                if te.stdout:
                    partial += _safe_decode(te.stdout)
                if te.stderr:
                    partial += _safe_decode(te.stderr)
                # Then try draining remaining bytes after kill
                try:
                    out, err = proc.communicate(timeout=5)
                    partial += _safe_decode(out) + _safe_decode(err)
                except subprocess.TimeoutExpired:
                    pass
                if partial and is_api_error(partial):
                    return (
                        False,
                        f"smoke test hit API error (after {timeout}s): {partial[:200]}",
                    )
                return (
                    False,
                    f"smoke test timed out after {timeout}s (MCP server may be misconfigured)",
                )
            except KeyboardInterrupt:
                _kill_process_group(proc)
                proc.communicate()
                raise
    except FileNotFoundError:
        return False, "smoke test failed: qwen binary not found"


def preflight_check(extended: bool = False, run_health: bool = True) -> bool:
    """Run pre-flight checks. Returns True if all critical checks pass."""
    print("Pre-flight checks:")
    all_ok = True

    # 1. qwen binary
    if check_qwen_binary():
        print("  [OK] qwen binary found")
    else:
        print("  [FAIL] qwen binary not found in PATH")
        print("         Install: https://github.com/QwenLM/qwen-code")
        return False

    # 2. zhtw-mcp binary
    exists, msg = check_mcp_binary()
    if exists:
        print(f"  [OK] {msg}")
    else:
        print(f"  [FAIL] {msg}")
        all_ok = False

    # 3. Extended: build binary if missing
    if extended and not exists:
        print("  Building release binary ...")
        r = subprocess.run(
            ["cargo", "build", "--release"],
            cwd=PROJECT_ROOT,
        )
        if r.returncode != 0:
            print("  [FAIL] cargo build --release failed")
            return False
        print("  [OK] build succeeded")
        all_ok = True  # binary now exists after successful build

    # 4. MCP server health check (direct JSON-RPC)
    health_ok = False
    if run_health and (exists or (extended and all_ok)):
        hr = mcp_health_check()
        if hr.ok:
            health_ok = True
            print(
                f"  [OK] MCP health: {hr.server_name} ({len(hr.tools)} tools, {hr.elapsed:.1f}s)"
            )
        else:
            for err in hr.errors:
                print(f"  [FAIL] MCP health: {err}")
            all_ok = False

    # 5. Extended: check MCP registration
    if extended:
        try:
            proc = subprocess.run(
                ["qwen", "mcp", "list"],
                capture_output=True,
                text=True,
                timeout=15,
            )
            output = proc.stdout + proc.stderr
            if "zhtw" in output.lower():
                print("  [OK] zhtw-mcp registered in qwen")
            else:
                print("  [WARN] zhtw-mcp not found in qwen mcp list")
                print("         Register: qwen mcp add zhtw-mcp <path-to-binary>")
        except (FileNotFoundError, subprocess.TimeoutExpired):
            print("  [WARN] could not check MCP registration")

    # 6. Smoke test (API errors downgraded to warning -- transient)
    ok, msg = smoke_test()
    if ok:
        print(f"  [OK] {msg}")
    elif "API error" in msg or "rate limit" in msg.lower() or "quota" in msg.lower():
        print(f"  [WARN] {msg} (transient, continuing)")
    elif health_ok and "timed out" in msg:
        # MCP server verified healthy via direct JSON-RPC; smoke test timeout
        # is likely a qwen-side issue (API quota, network, etc.), not MCP.
        print(f"  [WARN] {msg} (MCP server healthy, qwen-side issue likely)")
    else:
        print(f"  [FAIL] {msg}")
        print("         Check: qwen mcp list | grep zhtw")
        all_ok = False

    print()
    return all_ok


# ---------------------------------------------------------------------------
# Diagnostic summary
# ---------------------------------------------------------------------------


@dataclass
class TestResult:
    name: str
    status: Status
    elapsed: float
    reasons: list[str] = field(default_factory=list)


def print_diagnostics(results: list[TestResult]) -> None:
    """Analyze failure patterns and print actionable suggestions."""
    timeouts = [r for r in results if r.status == Status.TIMEOUT]
    crashes = [r for r in results if r.status == Status.CRASH]
    fails = [r for r in results if r.status == Status.FAIL]
    total_failures = len(timeouts) + len(crashes) + len(fails)

    if total_failures == 0:
        return

    print("\nDiagnostics:")

    if len(timeouts) == total_failures:
        print("  All failures are timeouts.")
        print("  -> MCP server may be misconfigured or unresponsive.")
        print("  -> Run: python3 scripts/test-mcp-qwen.py --health")
        print("  -> Check: qwen mcp list | grep zhtw")
        print("  -> Try: cargo build --release && qwen mcp add zhtw-mcp ...")
    elif len(crashes) == total_failures:
        print("  All failures are crashes (non-zero exit codes).")
        print("  -> zhtw-mcp binary may be broken.")
        print("  -> Check: cargo build --release")
        print("  -> Check: cargo test")
    elif timeouts and crashes:
        print("  Mix of timeouts and crashes detected.")
        print("  -> Server instability likely. Rebuild and re-register:")
        print("     cargo build --release && qwen mcp add zhtw-mcp ...")
    elif crashes and fails:
        print(f"  {len(crashes)} crash(es) and {len(fails)} assertion failure(s).")
        print("  -> Fix crashes first: cargo build --release && cargo test")
        print("  -> Then review assertion failures with -v flag.")
    elif fails:
        print("  Some tests failed keyword matching.")
        print("  -> Review output above for expected vs actual content.")
        if timeouts:
            print(
                f"  -> {len(timeouts)} test(s) also timed out -- consider --timeout increase."
            )


def print_timing_summary(results: list[TestResult]) -> None:
    """Print per-test timings and highlight the slowest."""
    if not results:
        return

    total = sum(r.elapsed for r in results)
    slowest = max(results, key=lambda r: r.elapsed)

    print("\nTiming:")
    for r in results:
        marker = " <-- slowest" if r is slowest and len(results) > 1 else ""
        print(f"  {r.name:25s} {r.elapsed:6.1f}s  {r.status.value}{marker}")
    print(f"  {'':25s} {'─' * 6}")
    print(f"  {'total':25s} {total:6.1f}s")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Test zhtw-mcp MCP tools via Qwen Code CLI"
    )
    parser.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        help="print full qwen output for each test",
    )
    parser.add_argument(
        "-k",
        "--filter",
        type=str,
        default="",
        help="run only tests whose name contains this substring",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=90,
        help="per-test timeout in seconds (default: 90)",
    )
    parser.add_argument(
        "--retries",
        type=int,
        default=1,
        help="retry count on API errors (default: 1, 0 = fail-fast)",
    )
    parser.add_argument(
        "--build",
        action="store_true",
        help="run cargo build --release before testing",
    )
    parser.add_argument(
        "--diagnose",
        action="store_true",
        help="run extended pre-flight diagnostics before tests",
    )
    parser.add_argument(
        "--no-preflight",
        action="store_true",
        help="skip pre-flight checks (not recommended)",
    )
    parser.add_argument(
        "--health",
        action="store_true",
        help="run MCP server health check only (no qwen needed)",
    )
    args = parser.parse_args()
    args.retries = max(0, args.retries)

    try:
        # Health-check-only mode
        if args.health:
            hr = mcp_health_check(timeout=30)
            print_health_report(hr)
            sys.exit(0 if hr.ok else 1)

        # Optional build step
        if args.build:
            print("Building release binary ...", flush=True)
            r = subprocess.run(
                ["cargo", "build", "--release"],
                cwd=PROJECT_ROOT,
            )
            if r.returncode != 0:
                print("Build failed, aborting.")
                sys.exit(1)
            print()

        # Pre-flight checks
        if not args.no_preflight:
            if not preflight_check(extended=args.diagnose):
                print(
                    "Pre-flight failed. Fix issues above or use --no-preflight to skip."
                )
                sys.exit(1)
        else:
            # Minimal check: qwen binary exists
            if not check_qwen_binary():
                print("Error: 'qwen' not found in PATH.", file=sys.stderr)
                sys.exit(1)

        # Filter tests
        tests = TESTS
        if args.filter:
            tests = [t for t in tests if args.filter.lower() in t.name.lower()]
            if not tests:
                print(f"No tests match filter: {args.filter!r}")
                sys.exit(1)

        # Run
        results: list[TestResult] = []

        print(f"Running {len(tests)} test(s) ...\n")

        for i, tc in enumerate(tests, 1):
            label = f"[{i}/{len(tests)}] {tc.name}"
            print(f"  {label} ...", end=" ", flush=True)

            test_timeout = tc.timeout if tc.timeout is not None else args.timeout
            output, elapsed, rc = run_qwen(
                tc.prompt, test_timeout, retries=args.retries
            )

            status, reasons = classify_result(tc, output, rc)
            results.append(TestResult(tc.name, status, elapsed, reasons))

            status_str = status.value
            if status == Status.CRASH:
                status_str = f"CRASH (rc={rc})"

            print(f"{status_str}  ({elapsed:.1f}s)")

            # Show output on failure or verbose
            show_output = args.verbose or status in (Status.FAIL, Status.CRASH)
            if show_output and output.strip():
                lines = output.strip().splitlines()
                limit = len(lines) if args.verbose else min(20, len(lines))
                for line in lines[:limit]:
                    print(f"    | {line}")
                if limit < len(lines):
                    print(f"    | ... ({len(lines) - limit} more lines)")
            if reasons and status not in (Status.PASS, Status.SKIP):
                for r in reasons:
                    print(f"    ! {r}")
            if show_output or (reasons and status not in (Status.PASS, Status.SKIP)):
                print()

        # Summary
        counts = {s: 0 for s in Status}
        for r in results:
            counts[r.status] += 1

        print("-" * 60)
        parts = [f"{counts[Status.PASS]} passed"]
        if counts[Status.FAIL]:
            parts.append(f"{counts[Status.FAIL]} failed")
        if counts[Status.TIMEOUT]:
            parts.append(f"{counts[Status.TIMEOUT]} timed out")
        if counts[Status.CRASH]:
            parts.append(f"{counts[Status.CRASH]} crashed")
        if counts[Status.SKIP]:
            parts.append(f"{counts[Status.SKIP]} skipped")
        parts.append(f"{len(results)} total")
        print(f"Results: {', '.join(parts)}")

        # Failure details
        failures = [
            r
            for r in results
            if r.status in (Status.FAIL, Status.TIMEOUT, Status.CRASH)
        ]
        if failures:
            print("\nFailures:")
            for r in failures:
                print(f"  {r.name} ({r.status.value}):")
                for reason in r.reasons:
                    print(f"    - {reason}")

        # Diagnostics and timing
        print_diagnostics(results)
        print_timing_summary(results)

        # All-skipped means no actionable results -- treat as failure
        all_skipped = all(r.status == Status.SKIP for r in results)
        exit_code = 0 if (not failures and not all_skipped) else 1
        sys.exit(exit_code)

    except KeyboardInterrupt:
        print("\nInterrupted -- exiting.", file=sys.stderr)
        sys.exit(130)


if __name__ == "__main__":
    main()
