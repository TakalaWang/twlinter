// Span-level judgment cache (51.4).
//
// Persists LLM disambiguation results so repeated encounters of the same
// term in similar contexts skip the LLM entirely.  Keyed on a 9-field
// composite: ruleset_hash, judgment_prompt_version, local_disambig_version,
// profile, content_type, normalized_context, ambiguous_term,
// candidate_set_hash, english_anchor.
//
// Storage: ~/.config/zhtw-mcp/judgment_cache.json (same pattern as
// override/suppression stores).  Schema-versioned with backup-and-reset.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Current schema version.  Bumping this invalidates all cached entries.
const SCHEMA_VERSION: u32 = 2;

/// Judgment prompt version.  Bump when the sampling prompt template changes.
pub const JUDGMENT_PROMPT_VERSION: u32 = 1;

/// Local disambiguation version.  Bump when Tier 2 scoring logic changes.
pub const LOCAL_DISAMBIG_VERSION: u32 = 1;

/// Default TTL: 30 days.
const DEFAULT_TTL_SECS: u64 = 30 * 24 * 3600;

/// Maximum TTL: 365 days (clamped to prevent overflow).
const MAX_TTL_SECS: u64 = 365 * 24 * 3600;

/// Maximum number of cache entries.  Oldest-expiry entries are evicted when
/// this limit is exceeded.
const MAX_ENTRIES: usize = 10_000;

/// Composite cache key (9 fields).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JudgmentKey {
    pub ruleset_hash: String,
    pub judgment_prompt_version: u32,
    pub local_disambig_version: u32,
    pub profile: String,
    pub content_type: String,
    pub normalized_context: String,
    pub ambiguous_term: String,
    pub candidate_set_hash: String,
    /// English anchor from the rule (e.g. "program").  Prevents aliasing
    /// when two rules share the same surface term but differ in meaning.
    pub english_anchor: String,
}

/// Cached judgment value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgmentValue {
    /// The chosen replacement term, if any.  None means rejection.
    pub chosen_replacement: Option<String>,
    /// Confidence score from the LLM [0.0, 1.0].
    pub confidence: f32,
    /// Full explanation/rationale text from the LLM (not just hash).
    #[serde(default)]
    pub explanation: String,
    /// Unix timestamp when this entry was created.
    pub created_at: u64,
    /// Unix timestamp when this entry expires.
    pub expires_at: u64,
    /// Model family that produced this judgment.
    pub model_family: String,
}

/// Persistent on-disk store.
#[derive(Debug, Serialize, Deserialize)]
struct CacheStore {
    schema_version: u32,
    entries: HashMap<String, JudgmentValue>,
}

impl Default for CacheStore {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            entries: HashMap::new(),
        }
    }
}

/// Normalize context for cache keying: strip all Unicode whitespace,
/// truncate to +-40 chars around center, include normalization version prefix.
pub fn normalize_context_for_cache(context: &str) -> String {
    let filtered: String = context.chars().filter(|c| !c.is_whitespace()).collect();
    let char_count = filtered.chars().count();
    let trimmed = if char_count <= 80 {
        filtered
    } else {
        let center = char_count / 2;
        let start = center.saturating_sub(40);
        let end = (center + 40).min(char_count);
        filtered.chars().skip(start).take(end - start).collect()
    };
    // Prefix with normalization version so format changes invalidate.
    format!("v1:{trimmed}")
}

/// Hash the candidate set (sorted suggestions) using blake3.
pub fn hash_candidate_set(suggestions: &[String]) -> String {
    let mut sorted = suggestions.to_vec();
    sorted.sort();
    let joined = sorted.join("\0");
    crate::audit::hash_hex(joined.as_bytes())
}

/// Make a serializable key string from a JudgmentKey.
fn key_string(key: &JudgmentKey) -> String {
    // Use blake3 of the JSON-serialized key for a fixed-length map key.
    let json = serde_json::to_vec(key).expect("JudgmentKey serialization is infallible");
    crate::audit::hash_hex(&json)
}

/// The judgment cache.
pub struct JudgmentCache {
    path: PathBuf,
    store: CacheStore,
    dirty: bool,
    ttl: Duration,
    /// Cache access statistics for telemetry (51.1).
    pub hits: u64,
    pub misses: u64,
}

impl JudgmentCache {
    /// Default cache file path.
    pub fn default_path() -> PathBuf {
        super::store::config_dir()
            .map(|d| d.join("zhtw-mcp").join("judgment_cache.json"))
            .unwrap_or_else(|| PathBuf::from("judgment_cache.json"))
    }

    /// Open or create the cache at the given path.
    /// Evicts expired entries on open to prevent unbounded growth.
    pub fn open(path: &Path) -> Self {
        let mut store = load_or_reset(path);
        // Evict expired entries eagerly on open.
        let now = now_unix();
        let before = store.entries.len();
        store.entries.retain(|_, v| v.expires_at > now);
        let evicted = before - store.entries.len();
        let mut dirty = evicted > 0;
        // Enforce max_entries cap: evict oldest-expiry entries.
        if store.entries.len() > MAX_ENTRIES {
            let mut by_expiry: Vec<(String, u64)> = store
                .entries
                .iter()
                .map(|(k, v)| (k.clone(), v.expires_at))
                .collect();
            by_expiry.sort_by_key(|(_, exp)| *exp);
            let to_remove = store.entries.len() - MAX_ENTRIES;
            for (k, _) in by_expiry.into_iter().take(to_remove) {
                store.entries.remove(&k);
            }
            dirty = true;
        }
        Self {
            path: path.to_path_buf(),
            store,
            dirty,
            ttl: Duration::from_secs(DEFAULT_TTL_SECS),
            hits: 0,
            misses: 0,
        }
    }

    /// Open at the default path.
    pub fn open_default() -> Self {
        Self::open(&Self::default_path())
    }

    /// Set TTL (clamped to [0, 365 days]).  TTL of 0 disables caching.
    pub fn set_ttl_days(&mut self, days: u32) {
        let secs = (days as u64).saturating_mul(24 * 3600).min(MAX_TTL_SECS);
        self.ttl = Duration::from_secs(secs);
    }

    /// Look up a cached judgment.  Returns None on miss or expired entry.
    pub fn get(&mut self, key: &JudgmentKey) -> Option<&JudgmentValue> {
        let ks = key_string(key);
        let now = now_unix();

        // Single entry lookup: check presence and expiry together.
        if let Some(v) = self.store.entries.get(&ks) {
            if v.expires_at <= now {
                // Expired: evict and count as miss.
                self.store.entries.remove(&ks);
                self.dirty = true;
                self.misses = self.misses.saturating_add(1);
                return None;
            }
            self.hits = self.hits.saturating_add(1);
            // Re-borrow after the expiry check (entry is still present).
            return self.store.entries.get(&ks);
        }

        self.misses = self.misses.saturating_add(1);
        None
    }

    /// Insert a judgment into the cache.
    pub fn insert(&mut self, key: &JudgmentKey, value: JudgmentValue) {
        if self.ttl.is_zero() {
            return; // caching disabled
        }
        let ks = key_string(key);
        self.store.entries.insert(ks, value);
        self.dirty = true;
    }

    /// Create a JudgmentValue with proper timestamps.
    pub fn make_value(
        &self,
        chosen_replacement: Option<String>,
        confidence: f32,
        explanation: String,
        model_family: String,
    ) -> JudgmentValue {
        let now = now_unix();
        let expires_at = now.saturating_add(self.ttl.as_secs());
        JudgmentValue {
            chosen_replacement,
            confidence,
            explanation,
            created_at: now,
            expires_at,
            model_family,
        }
    }

    /// Evict all expired entries.
    pub fn evict_expired(&mut self) {
        let now = now_unix();
        let before = self.store.entries.len();
        self.store.entries.retain(|_, v| v.expires_at > now);
        if self.store.entries.len() < before {
            self.dirty = true;
        }
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.store.entries.clear();
        self.dirty = true;
    }

    /// Number of entries in the cache.
    pub fn len(&self) -> usize {
        self.store.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.store.entries.is_empty()
    }

    /// Flush to disk if dirty.  Uses atomic write (tempfile + rename) to
    /// prevent truncated cache files on crash/power loss.
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string(&self.store) {
            Ok(json) => {
                // Atomic write: write to .tmp, then rename over the target.
                let tmp = self.path.with_extension("json.tmp");
                if std::fs::write(&tmp, &json).is_ok() {
                    if std::fs::rename(&tmp, &self.path).is_ok() {
                        self.dirty = false;
                    } else {
                        // Rename failed; try direct write as fallback.
                        let _ = std::fs::remove_file(&tmp);
                        if std::fs::write(&self.path, json).is_ok() {
                            self.dirty = false;
                        }
                    }
                }
            }
            Err(e) => tracing::warn!("failed to serialize judgment cache: {e}"),
        }
    }
}

impl Drop for JudgmentCache {
    fn drop(&mut self) {
        self.flush();
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Load the cache store from disk, or reset if schema version mismatch.
fn load_or_reset(path: &Path) -> CacheStore {
    let data = match std::fs::read_to_string(path) {
        Ok(d) => d,
        Err(_) => return CacheStore::default(),
    };
    match serde_json::from_str::<CacheStore>(&data) {
        Ok(store) if store.schema_version == SCHEMA_VERSION => store,
        result => {
            // Schema mismatch or parse error: backup and reset.
            if let Err(ref e) = result {
                tracing::warn!("failed to parse judgment cache: {e}, resetting");
            } else {
                tracing::info!("judgment cache schema mismatch, backing up and resetting");
            }
            let backup = path.with_extension("json.bak");
            let _ = std::fs::rename(path, &backup);
            CacheStore::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> JudgmentKey {
        JudgmentKey {
            ruleset_hash: "abc123".into(),
            judgment_prompt_version: JUDGMENT_PROMPT_VERSION,
            local_disambig_version: LOCAL_DISAMBIG_VERSION,
            profile: "base".into(),
            content_type: "markdown".into(),
            normalized_context: normalize_context_for_cache("some context around the term"),
            ambiguous_term: "進程".into(),
            candidate_set_hash: hash_candidate_set(&["行程".into(), "進程".into()]),
            english_anchor: "process".into(),
        }
    }

    #[test]
    fn insert_and_retrieve() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let mut cache = JudgmentCache::open(&path);
        let key = test_key();
        let value = cache.make_value(
            Some("行程".into()),
            0.9,
            "confirmed".into(),
            "claude".into(),
        );
        cache.insert(&key, value);
        assert_eq!(cache.len(), 1);
        let hit = cache.get(&key);
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().chosen_replacement.as_deref(), Some("行程"));
        assert_eq!(cache.hits, 1);
    }

    #[test]
    fn miss_on_different_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let mut cache = JudgmentCache::open(&path);
        let key = test_key();
        let value = cache.make_value(Some("行程".into()), 0.9, String::new(), "claude".into());
        cache.insert(&key, value);

        let mut key2 = test_key();
        key2.ruleset_hash = "different".into();
        assert!(cache.get(&key2).is_none());
        assert_eq!(cache.misses, 1);
    }

    #[test]
    fn expired_entry_evicted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let mut cache = JudgmentCache::open(&path);
        let key = test_key();
        // Insert with already-expired timestamp.
        let mut value = cache.make_value(Some("行程".into()), 0.9, String::new(), "claude".into());
        value.expires_at = 1; // long ago
        cache.store.entries.insert(key_string(&key), value);

        assert!(cache.get(&key).is_none());
        assert_eq!(cache.misses, 1);
        // Entry was lazily removed.
        assert!(cache.is_empty());
    }

    #[test]
    fn flush_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let key = test_key();
        {
            let mut cache = JudgmentCache::open(&path);
            let value = cache.make_value(Some("行程".into()), 0.8, "test".into(), "claude".into());
            cache.insert(&key, value);
            cache.flush();
        }
        // Reload from disk.
        let mut cache2 = JudgmentCache::open(&path);
        assert_eq!(cache2.len(), 1);
        let hit = cache2.get(&key);
        assert!(hit.is_some());
    }

    #[test]
    fn schema_mismatch_resets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        // Write a store with a different schema version.
        let old = serde_json::json!({
            "schema_version": 999,
            "entries": { "k1": { "chosen_replacement": null, "confidence": 0.5,
                "created_at": 0, "expires_at": 99999999999u64, "model_family": "x" } }
        });
        std::fs::write(&path, serde_json::to_string(&old).unwrap()).unwrap();
        let cache = JudgmentCache::open(&path);
        assert!(cache.is_empty());
        // Backup file should exist.
        assert!(path.with_extension("json.bak").exists());
    }

    #[test]
    fn clear_empties_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let mut cache = JudgmentCache::open(&path);
        let key = test_key();
        let value = cache.make_value(None, 0.1, String::new(), "claude".into());
        cache.insert(&key, value);
        assert_eq!(cache.len(), 1);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn normalize_context_strips_whitespace_and_trims() {
        let ctx = "  hello   world   with   lots   of   spaces  ";
        let normalized = normalize_context_for_cache(ctx);
        assert!(normalized.starts_with("v1:"));
        assert!(!normalized.contains(' '));
    }

    #[test]
    fn normalize_context_different_semantics_differ() {
        // Two contexts that differ semantically should produce different keys.
        let ctx1 = "不，好的我們可以這樣做";
        let ctx2 = "不好的我們可以這樣做";
        let n1 = normalize_context_for_cache(ctx1);
        let n2 = normalize_context_for_cache(ctx2);
        // The comma is preserved (not whitespace), so these differ.
        assert_ne!(n1, n2);
    }

    #[test]
    fn ttl_zero_disables_caching() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let mut cache = JudgmentCache::open(&path);
        cache.set_ttl_days(0);
        let key = test_key();
        let value = cache.make_value(Some("行程".into()), 0.9, String::new(), "claude".into());
        cache.insert(&key, value);
        assert!(cache.is_empty()); // not stored
    }

    #[test]
    fn candidate_set_hash_order_independent() {
        let h1 = hash_candidate_set(&["行程".into(), "進程".into()]);
        let h2 = hash_candidate_set(&["進程".into(), "行程".into()]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn evict_expired_removes_old_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let mut cache = JudgmentCache::open(&path);
        let key = test_key();
        let mut value = cache.make_value(Some("行程".into()), 0.9, String::new(), "claude".into());
        value.expires_at = 1; // expired
        cache.store.entries.insert(key_string(&key), value);
        assert_eq!(cache.len(), 1);
        cache.evict_expired();
        assert!(cache.is_empty());
    }
}
