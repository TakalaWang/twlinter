// Audit tracing: per-invocation reproducibility metadata.
//
// Each tool call generates a Trace with a unique trace_id (SHA-256 of
// timestamp + PID + counter + urandom seed), hashes of input/output text,
// and the ruleset hash. This enables deterministic replay.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use serde::Serialize;

/// Monotonic counter to guarantee uniqueness even within the same nanosecond.
static TRACE_SEQ: AtomicU64 = AtomicU64::new(0);

/// 16-byte random seed read once from /dev/urandom at first use.
/// Prevents trace_id collisions between processes with identical PID and clock
/// (common in containers where PID 1 restarts frequently).
static URANDOM_SEED: OnceLock<[u8; 16]> = OnceLock::new();

fn urandom_seed() -> &'static [u8; 16] {
    URANDOM_SEED.get_or_init(|| {
        let mut seed = [0u8; 16];
        // Best-effort: if /dev/urandom is unavailable (e.g., sandboxed), fall
        // back to all-zeros (still unique within a process via TRACE_SEQ).
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            use std::io::Read;
            let _ = f.read_exact(&mut seed);
        }
        seed
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct Trace {
    pub trace_id: String,
    pub ruleset_hash: String,
    pub input_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_hash: Option<String>,
    pub timestamp: String,
    pub tool: String,
    pub issue_count: usize,
}

impl Trace {
    pub fn new(tool: &str, ruleset_hash: &str, input: &str) -> Self {
        Self {
            trace_id: generate_trace_id(),
            ruleset_hash: ruleset_hash.to_owned(),
            input_hash: hash_hex(input.as_bytes()),
            output_hash: None,
            timestamp: now_iso8601(),
            tool: tool.to_owned(),
            issue_count: 0,
        }
    }

    pub fn with_issue_count(mut self, count: usize) -> Self {
        self.issue_count = count;
        self
    }

    pub fn with_output(mut self, output: &str) -> Self {
        self.output_hash = Some(hash_hex(output.as_bytes()));
        self
    }
}

pub fn hash_hex(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

/// Generate a unique trace ID from timestamp + PID + atomic counter, hashed
/// with SHA-256. Returns the first 32 hex characters (128 bits of entropy,
/// matching UUIDv4 uniqueness). No external RNG crate needed.
fn generate_trace_id() -> String {
    use std::process;
    use std::time::SystemTime;

    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = TRACE_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = process::id();

    let mut hasher = blake3::Hasher::new();
    hasher.update(&nanos.to_le_bytes());
    hasher.update(&pid.to_le_bytes());
    hasher.update(&seq.to_le_bytes());
    hasher.update(urandom_seed());
    // Take first 32 hex chars (128 bits) from the BLAKE3 output.
    hasher.finalize().to_hex()[..32].to_string()
}

fn now_iso8601() -> String {
    // Use UNIX_EPOCH + SystemTime for a dependency-free ISO 8601 timestamp.
    use std::time::SystemTime;
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let millis = dur.subsec_millis();
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Convert days since epoch to Y-M-D (simplified Gregorian).
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Algorithm from Howard Hinnant's chrono-compatible date library.
    days += 719_468;
    let era = days / 146_097;
    let doe = days % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_deterministic() {
        let a = hash_hex(b"hello");
        let b = hash_hex(b"hello");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn hash_known_value() {
        let h = hash_hex(b"hello");
        // BLAKE3 hash of "hello"
        assert_eq!(
            h,
            "ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f"
        );
    }

    #[test]
    fn trace_unique_ids() {
        let t1 = Trace::new("zhtw", "abc", "text");
        let t2 = Trace::new("zhtw", "abc", "text");
        assert_ne!(t1.trace_id, t2.trace_id);
        // Same input → same input_hash
        assert_eq!(t1.input_hash, t2.input_hash);
    }

    #[test]
    fn trace_output_hash() {
        let t = Trace::new("zhtw", "abc", "input")
            .with_output("output")
            .with_issue_count(3);
        assert!(t.output_hash.is_some());
        assert_eq!(t.issue_count, 3);
    }

    #[test]
    fn timestamp_format() {
        let ts = now_iso8601();
        // Basic format check: YYYY-MM-DDTHH:MM:SS.mmmZ
        assert_eq!(ts.len(), 24);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[19..20], ".");
    }
}
