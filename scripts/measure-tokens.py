#!/usr/bin/env python3
"""Measure token reduction across all Token Optimization features.

Uses tiktoken cl100k_base for approximate BPE token counts (cl100k_base proxy;
actual Claude tokenization may differ). Sends MCP protocol messages to the server
and measures response token costs.
"""

import json
import os
import subprocess
import sys
import tiktoken

# Resolve project root from this script's location.
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
PROJECT_ROOT = os.path.dirname(SCRIPT_DIR)

# cl100k_base is OpenAI's tokenizer (GPT-4); used here as a rough proxy since
# Anthropic does not publish a public tiktoken-compatible Claude tokenizer.
# Relative token savings between output modes remain directionally valid.
enc = tiktoken.get_encoding("cl100k_base")


def count_tokens(text: str) -> int:
    return len(enc.encode(text))


def run_mcp(requests: list[dict]) -> list[dict]:
    """Send JSON-RPC requests to the MCP server, return responses keyed by id."""
    init = {
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1.0"},
        },
    }
    notif = {"jsonrpc": "2.0", "method": "notifications/initialized"}
    lines = [json.dumps(init, ensure_ascii=False), json.dumps(notif)]
    for req in requests:
        lines.append(json.dumps(req, ensure_ascii=False))
    input_data = "\n".join(lines) + "\n"

    result = subprocess.run(
        ["cargo", "run", "--release", "--"],
        input=input_data,
        capture_output=True,
        text=True,
        cwd=PROJECT_ROOT,
    )

    if result.returncode != 0:
        raise RuntimeError(
            f"cargo run failed (exit {result.returncode}):\n{result.stderr}"
        )

    responses = {}
    for line in result.stdout.strip().split("\n"):
        if not line.strip():
            continue
        msg = json.loads(line)
        rid = msg.get("id")
        if rid is not None and rid != 0:
            responses[rid] = msg
    return responses


def extract_text(response: dict) -> str:
    """Extract the text content from an MCP tool response."""
    return response["result"]["content"][0]["text"]


def print_header(title: str):
    print(f"\n{'='*70}")
    print(f"  {title}")
    print(f"{'='*70}")


def print_row(label: str, tokens: int, baseline: int | None = None):
    if baseline is not None and baseline > 0:
        pct = (1 - tokens / baseline) * 100
        print(f"  {label:<30s}  {tokens:>6d} tokens  ({pct:+.1f}%)")
    else:
        print(f"  {label:<30s}  {tokens:>6d} tokens")


# ============================================================
# Test corpus: realistic zh-TW text with CN drift
# ============================================================

# Small: ~100 chars, 3 issues
SMALL = "這個軟件的內存很大，視頻也很清楚"

# Medium: ~500 chars, ~10 issues (typical AI-generated paragraph)
MEDIUM = (
    "這個軟件在服務器上運行時，內存佔用很高。"
    "我們需要優化數據庫的查詢性能，"
    "同時確保網絡連接的穩定性。"
    "用戶界面設計要考慮信息架構，"
    "讓用戶能夠快速找到需要的功能。"
    "硬件配置需要支持高並發的數據處理，"
    "打印服務和鼠標操作的響應速度也很重要。"
    "激活程序後，系統日誌會記錄所有操作。"
)

# Large: ~5KB, 10 issues embedded in clean text
CLEAN_PARA = (
    "台灣的科技產業發展迅速，半導體製造技術領先全球。"
    "台積電是全球最大的晶圓代工廠，其先進製程技術獲得國際客戶的高度肯定。"
    "台灣在人工智慧、雲端運算、物聯網等領域也有顯著的發展成果。"
    "政府積極推動數位轉型政策，鼓勵產業創新與國際合作。"
)
LARGE = CLEAN_PARA * 10 + MEDIUM


def measure_output_modes():
    """38.1: Compare full vs compact vs tabular output token costs."""
    print_header("38.1  Output Mode Token Comparison")

    for label, text in [
        ("Small (3 issues)", SMALL),
        ("Medium (~10 issues)", MEDIUM),
        ("Large (~10 issues, 5KB)", LARGE),
    ]:
        reqs = [
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "zhtw",
                    "arguments": {"text": text, "output": "full"},
                },
            },
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "zhtw",
                    "arguments": {"text": text, "output": "compact"},
                },
            },
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "zhtw",
                    "arguments": {"text": text, "output": "tabular"},
                },
            },
        ]
        responses = run_mcp(reqs)
        full_tokens = count_tokens(extract_text(responses[1]))
        compact_tokens = count_tokens(extract_text(responses[2]))
        tabular_tokens = count_tokens(extract_text(responses[3]))

        print(f"\n  [{label}]")
        print_row("full (JSON)", full_tokens)
        print_row("compact (JSON)", compact_tokens, full_tokens)
        print_row("tabular (TSV)", tabular_tokens, full_tokens)


def measure_fix_output():
    """38.2: Compare full text vs search_replace vs patch for fix responses."""
    print_header("38.2  Fix Output Token Comparison")

    for label, text in [
        ("Small (3 issues)", SMALL),
        ("Medium (~10 issues)", MEDIUM),
        ("Large (~10 issues, 5KB)", LARGE),
    ]:
        reqs = [
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "zhtw",
                    "arguments": {
                        "text": text,
                        "fix_mode": "lexical_safe",
                        "fix_output": "full",
                    },
                },
            },
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "zhtw",
                    "arguments": {
                        "text": text,
                        "fix_mode": "lexical_safe",
                        "fix_output": "search_replace",
                    },
                },
            },
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "zhtw",
                    "arguments": {
                        "text": text,
                        "fix_mode": "lexical_safe",
                        "fix_output": "patch",
                    },
                },
            },
        ]
        responses = run_mcp(reqs)
        full_tokens = count_tokens(extract_text(responses[1]))
        sr_tokens = count_tokens(extract_text(responses[2]))
        patch_tokens = count_tokens(extract_text(responses[3]))

        print(f"\n  [{label}]")
        print_row("full text", full_tokens)
        print_row("search_replace", sr_tokens, full_tokens)
        print_row("patch (JSON offsets)", patch_tokens, full_tokens)


def measure_prompt_compression():
    """38.3: Compare old vs new sampling prompt token costs."""
    print_header("38.3  Sampling Prompt Token Comparison")

    # Simulate old prompt (before optimization)
    context = "這個算法支持並行計算，能夠充分利用多核處理器的性能優勢"
    found = "並行"
    english = "parallelism"
    suggestions_old = "平行, 並行"
    suggestions_new = "平行, 並行"

    old_prompt = (
        f'Context: "{context}"\n\n'
        f"The term '{found}' (English: {english}) was found. "
        f"In Taiwan Traditional Chinese, the correct term could be: {suggestions_old}.\n\n"
        f"Based on the context, which term is correct? Reply with ONLY the correct "
        f"Chinese term, nothing else."
    )

    new_prompt = (
        f'"{context}"\n'
        f"'{found}'(en:{english}) zh-TW:{suggestions_new}\n"
        f"Correct term? If unsure:UNKNOWN"
    )

    old_tokens = count_tokens(old_prompt)
    new_tokens = count_tokens(new_prompt)

    print(f"\n  [Single disambiguation prompt]")
    print_row("old prompt", old_tokens)
    print_row("new prompt", new_tokens, old_tokens)

    # Old bulk confirm prompt
    terms_json = json.dumps(
        [
            {
                "id": 0,
                "found": "渲染",
                "english": "rendering",
                "context": "GPU渲染管線",
            },
            {
                "id": 1,
                "found": "實例",
                "english": "instance",
                "context": "建立一個實例",
            },
            {"id": 2, "found": "調用", "english": "call", "context": "API調用"},
        ],
        ensure_ascii=False,
    )

    old_bulk = (
        "You are a cross-strait Chinese terminology validator. "
        "For each term below, determine if the English translation confirms "
        "it is being used as the cross-strait (Mainland China) variant rather "
        "than the Taiwan standard term.\n\n"
        f"Terms: {terms_json}\n\n"
        'Reply with ONLY a JSON object mapping each "id" (as string key) to '
        "true (confirmed cross-strait usage) or false (not confirmed). "
        'Example: {"0": true, "1": false}\n'
        "No explanation, no markdown, just the JSON object."
    )

    new_bulk = (
        "Per term: true=mainland CN, false=not.\n"
        f"{terms_json}\n"
        'JSON:{"0":true,"1":false}'
    )

    old_bulk_tokens = count_tokens(old_bulk)
    new_bulk_tokens = count_tokens(new_bulk)

    print(f"\n  [Bulk confirm prompt (3 terms)]")
    print_row("old prompt", old_bulk_tokens)
    print_row("new prompt", new_bulk_tokens, old_bulk_tokens)

    # maxTokens budget savings (response side)
    print(f"\n  [maxTokens budget]")
    print(f"  {'old maxTokens (disambig)':<30s}  {'100':>6s} tokens")
    print(f"  {'new maxTokens (disambig)':<30s}  {'32':>6s} tokens  (-68.0%)")
    print(f"  {'old maxTokens (bulk)':<30s}  {'1024':>6s} tokens")
    print(f"  {'new maxTokens (bulk)':<30s}  {'128':>6s} tokens  (-87.5%)")


def measure_combined():
    """Combined: tabular + search_replace vs full + full (best case)."""
    print_header("Combined: Tabular + SearchReplace vs Full + Full")

    for label, text in [
        ("Medium (~10 issues)", MEDIUM),
        ("Large (~10 issues, 5KB)", LARGE),
    ]:
        reqs = [
            # Baseline: full output + full fix text
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "zhtw",
                    "arguments": {
                        "text": text,
                        "output": "full",
                        "fix_mode": "lexical_safe",
                        "fix_output": "full",
                    },
                },
            },
            # Optimized: tabular output + search_replace fix
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "zhtw",
                    "arguments": {
                        "text": text,
                        "output": "tabular",
                        "fix_mode": "lexical_safe",
                        "fix_output": "search_replace",
                    },
                },
            },
        ]
        responses = run_mcp(reqs)
        baseline = count_tokens(extract_text(responses[1]))
        optimized = count_tokens(extract_text(responses[2]))

        print(f"\n  [{label}]")
        print_row("full + full", baseline)
        print_row("tabular + search_replace", optimized, baseline)


if __name__ == "__main__":
    print("Token Optimization Measurement Report")
    print(f"Tokenizer: cl100k_base (OpenAI proxy; actual Claude counts may differ)")
    print(f"Measurement: approximate tiktoken BPE token count")

    measure_output_modes()
    measure_fix_output()
    measure_prompt_compression()
    measure_combined()

    print(f"\n{'='*70}")
    print("  Done.")
    print(f"{'='*70}")
