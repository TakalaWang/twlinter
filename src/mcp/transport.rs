// Synchronous stdio transport for MCP JSON-RPC 2.0.
//
// Reads newline-delimited JSON from stdin via a background reader thread,
// dispatches to the tool server, writes JSON responses to stdout.
//
// The reader thread enables non-blocking reads with timeout, which is
// required for sampling support: the server can send a sampling/createMessage
// request and wait for the client's response without blocking the entire
// transport indefinitely.
//
// During a sampling window, the bridge may consume messages from the shared
// channel that belong to the main dispatch loop (notifications, other
// requests).  These are returned as "spillover" and re-processed by the main
// loop after the bridge is dropped.

use std::collections::VecDeque;
use std::io::{self, BufRead, Read, Write};
use std::str;
use std::sync::mpsc;
use std::thread;

use anyhow::Result;

use super::sampling::{SamplingBridge, DEFAULT_SAMPLING_BUDGET, DEFAULT_SAMPLING_TIMEOUT};
use super::tools::Server;
use super::types::{
    JsonRpcRequest, JsonRpcResponse, RequestId, TransportError, INTERNAL_ERROR, INVALID_REQUEST,
    PARSE_ERROR,
};

/// Maximum line length we'll accept from stdin (4 MiB payload).
/// Prevents memory exhaustion from malformed input.
const MAX_LINE_BYTES: u64 = 4 * 1024 * 1024;

/// Messages from the stdin reader thread.
#[derive(Debug)]
pub(crate) enum StdinMsg {
    /// A complete, non-empty line of text.
    Line(String),
    /// A line that exceeded the maximum allowed size.
    TooLong,
    /// A line that contains malformed UTF-8 text
    MalformedUtf8(str::Utf8Error),
}

/// Result of a bounded line read (internal to the reader thread).
enum ReadLine {
    /// A validated, trimmed line. UTF-8 is checked exactly once, here, so the
    /// caller neither re-validates nor risks a panic on an invalid boundary.
    Line(String),
    Eof,
    TooLong,
    MalformedUtf8(str::Utf8Error),
}

/// Serialize a response as JSON and write it to stdout, followed by a newline.
/// Writes directly to the buffered writer without an intermediate String.
fn send(out: &mut impl Write, resp: &JsonRpcResponse) -> Result<()> {
    serde_json::to_writer(&mut *out, resp)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

/// Read a single line from reader, bounded to MAX_LINE_BYTES.
///
/// Reads into `raw` (cleared first) so the caller can reuse the read buffer
/// across calls.  On success the line is validated as UTF-8 once and returned
/// trimmed as `ReadLine::Line(String)`, so the caller never re-validates.
fn read_bounded_line(reader: &mut impl BufRead, raw: &mut Vec<u8>) -> io::Result<ReadLine> {
    raw.clear();
    let n = reader
        .by_ref()
        .take(MAX_LINE_BYTES + 1)
        .read_until(b'\n', raw)?;

    if n == 0 {
        return Ok(ReadLine::Eof);
    }

    if !raw.ends_with(b"\n") && n as u64 > MAX_LINE_BYTES {
        drain_until_newline(reader)?;
        return Ok(ReadLine::TooLong);
    }

    match str::from_utf8(raw) {
        Ok(s) => Ok(ReadLine::Line(s.trim().to_owned())),
        Err(e) => Ok(ReadLine::MalformedUtf8(e)),
    }
}

/// Consume and discard bytes from reader until a newline or EOF.
fn drain_until_newline(reader: &mut impl BufRead) -> io::Result<()> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            reader.consume(pos + 1);
            break;
        }
        let len = available.len();
        reader.consume(len);
    }
    Ok(())
}

/// Run the MCP server on stdin/stdout until EOF.
///
/// Spawns a background thread to read stdin, enabling timeout-based reads
/// for sampling support.  The main thread processes requests and writes
/// responses to stdout.
///
/// Messages consumed by the sampling bridge during a tools/call dispatch
/// are returned as spillover and re-processed before reading new input.
pub fn run_stdio(server: &mut Server) -> Result<()> {
    let (line_tx, line_rx) = mpsc::channel::<StdinMsg>();
    // Unbounded by design: the tracing layer's on_event sends from this same
    // thread during dispatch, and we drain on this thread between requests. A
    // bounded channel would deadlock if it filled mid-dispatch (sender blocks,
    // receiver can't run). Per-request log volume is small, so it can't grow.
    let (log_tx, log_rx) = mpsc::channel::<crate::trace::McpLogMessage>();

    // Background stdin reader: reads bounded lines and sends them to the channel.
    // The raw buffer is allocated once and reused across reads.
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        let mut raw: Vec<u8> = Vec::new();
        loop {
            match read_bounded_line(&mut reader, &mut raw) {
                Ok(ReadLine::Eof) => break,
                Ok(ReadLine::Line(text)) => {
                    // Already validated and trimmed by read_bounded_line.
                    if !text.is_empty() && line_tx.send(StdinMsg::Line(text)).is_err() {
                        break; // receiver dropped
                    }
                }
                Ok(ReadLine::TooLong) => {
                    if line_tx.send(StdinMsg::TooLong).is_err() {
                        break;
                    }
                }
                Ok(ReadLine::MalformedUtf8(e)) => {
                    if line_tx.send(StdinMsg::MalformedUtf8(e)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!("stdin reader: {e}");
                    break;
                }
            }
        }
    });

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let mut pending: VecDeque<StdinMsg> = VecDeque::new();
    let mut mcp_logging = false;

    // Drain spillover before blocking on the channel; a closed channel
    // produces None and breaks the loop (EOF).
    while let Some(msg) = pending.pop_front().or_else(|| line_rx.recv().ok()) {
        let line = match msg {
            StdinMsg::Line(l) => l,
            StdinMsg::TooLong => {
                tracing::warn!("line too long, returning error");
                let resp =
                    JsonRpcResponse::error(None, INVALID_REQUEST, "request too large".into());
                if mcp_logging {
                    drain_mcp_logs(&mut writer, &log_rx)?;
                }
                send(&mut writer, &resp)?;
                continue;
            }
            StdinMsg::MalformedUtf8(e) => {
                tracing::warn!("line contains malformed UTF-8 character(s), returning error: {e}");
                let resp = JsonRpcResponse::error(
                    None,
                    PARSE_ERROR,
                    "request contains malformed UTF-8 character(s)".into(),
                );
                if mcp_logging {
                    drain_mcp_logs(&mut writer, &log_rx)?;
                }
                send(&mut writer, &resp)?;
                continue;
            }
        };

        // Parse and validate the JSON-RPC request via shared parser.
        // Distinguishes malformed JSON (-32700) from invalid request (-32600)
        // and silently discards stale sampling responses (no id, no method).
        let mut req: JsonRpcRequest = match super::types::parse_jsonrpc_line(&line) {
            Ok(r) => r,
            Err(TransportError::StaleResponse) => {
                tracing::debug!("discarding stale response-shaped message (no id)");
                continue;
            }
            Err(e) => {
                tracing::warn!("{e}");
                if mcp_logging {
                    drain_mcp_logs(&mut writer, &log_rx)?;
                }
                if let Some(resp) = e.into_response(None) {
                    send(&mut writer, &resp)?;
                }
                continue;
            }
        };

        // Notifications (no id) get no response per JSON-RPC spec. Handler
        // panics are caught inside dispatch() (the scanner processes untrusted
        // text), so one bad request replies -32603 instead of killing the loop.
        let (resp, spillover) = dispatch(server, &mut req, &line_rx, &mut writer);
        if server.supports_logging() && !mcp_logging {
            crate::trace::set_mcp_log_sender(Some(log_tx.clone()));
            mcp_logging = true;
        }
        // Logs accrue synchronously during dispatch on this same thread, so a
        // single drain afterward catches everything; emit them before the
        // response so clients see notifications in causal order.
        drain_mcp_logs(&mut writer, &log_rx)?;
        if let Some(resp) = resp {
            send(&mut writer, &resp)?;
        }
        for msg in spillover {
            pending.push_back(msg);
        }
    }

    crate::trace::set_mcp_log_sender(None);
    Ok(())
}

fn drain_mcp_logs(
    out: &mut impl Write,
    log_rx: &mpsc::Receiver<crate::trace::McpLogMessage>,
) -> Result<()> {
    let mut wrote = false;
    while let Ok(msg) = log_rx.try_recv() {
        serde_json::to_writer(
            &mut *out,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/message",
                "params": msg,
            }),
        )?;
        out.write_all(b"\n")?;
        wrote = true;
    }
    if wrote {
        out.flush()?;
    }
    Ok(())
}

/// Message for the -32603 returned when a request handler panics.
const HANDLER_PANIC_MSG: &str = "internal error: request handler panicked";

/// Run `f`, catching a panic as `Err(())`. The default panic hook still logs
/// the payload to stderr; stdout stays reserved for JSON-RPC. `AssertUnwindSafe`
/// is sound here: a caught panic may leave `Server`'s caches/counters partially
/// updated, but `HashMap`/`Vec` stay structurally valid across the unwind, so
/// later requests see stale-but-consistent state rather than a dead server.
fn catch_panic<T>(f: impl FnOnce() -> T) -> Result<T, ()> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|_| ())
}

/// Map a caught handler panic to an optional error response: `Some(-32603)`
/// for a request (has id), `None` for a notification (no id, no reply per spec).
fn panic_response(id: &Option<RequestId>) -> Option<JsonRpcResponse> {
    tracing::error!("request handler panicked");
    id.clone()
        .map(|id| JsonRpcResponse::error(Some(id), INTERNAL_ERROR, HANDLER_PANIC_MSG.into()))
}

fn dispatch(
    server: &mut Server,
    req: &mut JsonRpcRequest,
    line_rx: &mpsc::Receiver<StdinMsg>,
    writer: &mut impl Write,
) -> (Option<JsonRpcResponse>, Vec<StdinMsg>) {
    // Pre-init routing (initialize, ping, notifications, rejection).
    match catch_panic(|| server.dispatch_preinit(req)) {
        Ok(Some(resp)) => return (resp, Vec::new()),
        Ok(None) => {}
        Err(()) => return (panic_response(&req.id), Vec::new()),
    }

    // tools/call needs a sampling bridge; all others delegate to dispatch_method.
    if req.method == "tools/call" {
        // The bridge lives outside the catch so its buffered spillover (unrelated
        // client messages read while awaiting a sampling reply) is reclaimed even
        // when the tool handler panics mid-call.
        let mut bridge = if server.supports_sampling() {
            Some(SamplingBridge::new(
                writer,
                line_rx,
                DEFAULT_SAMPLING_TIMEOUT,
                DEFAULT_SAMPLING_BUDGET,
            ))
        } else {
            None
        };
        let resp = match catch_panic(|| server.handle_tools_call(req, bridge.as_mut())) {
            Ok(resp) => Some(resp),
            Err(()) => panic_response(&req.id),
        };
        let spillover = bridge.map_or_else(Vec::new, SamplingBridge::into_spillover);
        (resp, spillover)
    } else {
        match catch_panic(|| server.dispatch_method(req)) {
            Ok(resp) => (resp, Vec::new()),
            Err(()) => (panic_response(&req.id), Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // -- read_bounded_line tests --

    #[test]
    fn rbl_eof_returns_eof() {
        let mut reader = Cursor::new(b"" as &[u8]);
        let mut raw = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::Eof
        ));
    }

    #[test]
    fn rbl_simple_line_with_newline() {
        let mut reader = Cursor::new(b"hello\n" as &[u8]);
        let mut raw = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::Line(_)
        ));
        let text = str::from_utf8(&raw).unwrap();
        assert_eq!(text.trim(), "hello");
    }

    #[test]
    fn rbl_line_without_trailing_newline() {
        // Last line in stream with no newline — still valid
        let mut reader = Cursor::new(b"hello" as &[u8]);
        let mut raw = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::Line(_)
        ));
        assert_eq!(str::from_utf8(&raw).unwrap(), "hello");
    }

    #[test]
    fn rbl_empty_line() {
        let mut reader = Cursor::new(b"\n" as &[u8]);
        let mut raw = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::Line(_)
        ));
        assert_eq!(raw, b"\n");
        assert!(str::from_utf8(&raw).unwrap().trim().is_empty());
    }

    #[test]
    fn rbl_utf8_boundary_at_max() {
        // CJK char '中' (3 bytes E4 B8 AD) at the boundary
        let padding_len = (MAX_LINE_BYTES as usize) - 4; // 3-byte char + newline
        let mut data = vec![b'a'; padding_len];
        data.extend_from_slice("中".as_bytes());
        data.push(b'\n');
        assert_eq!(data.len(), MAX_LINE_BYTES as usize);

        let mut reader = Cursor::new(data);
        let mut raw = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::Line(_)
        ));
        let text = str::from_utf8(&raw).unwrap();
        assert!(text.trim().ends_with('中'));
    }

    #[test]
    fn rbl_utf8_multibyte_straddling_limit() {
        // 4-byte emoji that would straddle MAX_LINE_BYTES if we used
        // BufRead::read_line instead of raw bytes.  read_until on raw
        // bytes reads the full sequence without InvalidData.
        let padding_len = (MAX_LINE_BYTES as usize) - 2; // emoji is 4 bytes, total > limit
        let mut data = vec![b'a'; padding_len];
        data.extend_from_slice("🦀".as_bytes()); // 4 bytes
        data.push(b'\n');
        // total = padding_len + 4 + 1 = MAX_LINE_BYTES + 3, exceeds limit
        // but since it has a newline, read_until stops at it.
        // However, the take() limit is MAX_LINE_BYTES + 1, so only
        // MAX_LINE_BYTES + 1 bytes are read — the emoji is split.
        // Since there's no newline within the first MAX_LINE_BYTES+1 bytes,
        // and n > MAX_LINE_BYTES, this is TooLong.

        let mut reader = Cursor::new(data);
        let mut raw = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::TooLong
        ));
    }

    #[test]
    fn rbl_too_long_no_newline() {
        let data = vec![b'x'; (MAX_LINE_BYTES as usize) + 100];
        let mut reader = Cursor::new(data);
        let mut raw = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::TooLong
        ));
    }

    #[test]
    fn rbl_too_long_drain_to_next_newline() {
        // Oversized line followed by a valid line. After TooLong + drain,
        // the next read should get the second line.
        let mut data = vec![b'x'; (MAX_LINE_BYTES as usize) + 100];
        data.push(b'\n');
        data.extend_from_slice(b"valid\n");

        let mut reader = Cursor::new(data);
        let mut raw = Vec::new();

        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::TooLong
        ));
        // Next read should yield the valid line
        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::Line(_)
        ));
        assert_eq!(str::from_utf8(&raw).unwrap().trim(), "valid");
    }

    #[test]
    fn rbl_malformed_utf8() {
        let data = vec![0xFF, 0xFE, b'\n'];
        let mut reader = Cursor::new(data);
        let mut raw = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::MalformedUtf8(_)
        ));
    }

    #[test]
    fn rbl_buffer_reused_across_calls() {
        let data = b"line1\nline2\n";
        let mut reader = Cursor::new(&data[..]);
        let mut raw = Vec::new();

        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::Line(_)
        ));
        assert_eq!(str::from_utf8(&raw).unwrap().trim(), "line1");

        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::Line(_)
        ));
        assert_eq!(str::from_utf8(&raw).unwrap().trim(), "line2");

        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::Eof
        ));
    }

    #[test]
    fn rbl_mixed_valid_and_empty_lines() {
        let data = b"first\n\nsecond\n";
        let mut reader = Cursor::new(&data[..]);
        let mut raw = Vec::new();

        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::Line(_)
        ));
        assert_eq!(str::from_utf8(&raw).unwrap().trim(), "first");

        // Empty line
        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::Line(_)
        ));
        assert!(str::from_utf8(&raw).unwrap().trim().is_empty());

        assert!(matches!(
            read_bounded_line(&mut reader, &mut raw).unwrap(),
            ReadLine::Line(_)
        ));
        assert_eq!(str::from_utf8(&raw).unwrap().trim(), "second");
    }

    // -- panic recovery tests --

    #[test]
    fn panic_response_request_gets_internal_error() {
        let resp = panic_response(&Some(RequestId::Int(7))).expect("request must get a response");
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["error"]["code"], INTERNAL_ERROR);
        assert!(v.get("result").is_none());
    }

    #[test]
    fn panic_response_notification_gets_no_response() {
        // No id => notification => no reply, per JSON-RPC spec.
        assert!(panic_response(&None).is_none());
    }

    #[test]
    fn catch_panic_maps_panic_to_err() {
        assert_eq!(catch_panic(|| 41 + 1), Ok(42));
        assert_eq!(catch_panic(|| panic!("boom")), Err(()));
    }

    // -- send() tests --

    #[test]
    fn send_writes_json_newline() {
        let resp = JsonRpcResponse::success(
            Some(super::super::types::RequestId::Int(1)),
            serde_json::json!({"ok": true}),
        );
        let mut buf: Vec<u8> = Vec::new();
        send(&mut buf, &resp).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.ends_with('\n'));
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["result"]["ok"], true);
    }

    #[test]
    fn send_error_response_format() {
        let resp = JsonRpcResponse::error(
            Some(super::super::types::RequestId::Str("abc".into())),
            PARSE_ERROR,
            "bad json".into(),
        );
        let mut buf: Vec<u8> = Vec::new();
        send(&mut buf, &resp).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(parsed["id"], "abc");
        assert_eq!(parsed["error"]["code"], PARSE_ERROR);
        assert!(parsed.get("result").is_none());
    }

    #[test]
    fn send_propagates_writer_failure() {
        struct FailWriter;
        impl Write for FailWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken"))
            }
        }
        let resp = JsonRpcResponse::success(None, serde_json::json!(null));
        assert!(send(&mut FailWriter, &resp).is_err());
    }
}
