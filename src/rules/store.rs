use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;

use super::ruleset::{CaseRule, SpellingRule};

/// Schema version. Bump whenever the JSON override format or startup
/// contract changes.
///
/// History:
///   1 — implicit (sled era, no version stored)
///   2 — sled era: SpellingRule gained exceptions: Option<Vec<String>>
///   3 — JSON-file overrides, sled removed
pub const SCHEMA_VERSION: u32 = 3;

/// Acquire an exclusive advisory lock on a lockfile adjacent to the target path.
///
/// Returns the locked `File` handle; the lock is released when the handle is
/// dropped. This prevents concurrent MCP server instances from racing on the
/// same JSON file during read-modify-write sequences.
fn acquire_lock(path: &Path) -> Result<std::fs::File> {
    let lock_path = path.with_extension("lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open lock file: {}", lock_path.display()))?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("acquire lock: {}", lock_path.display()))?;
    Ok(lock_file)
}

/// Metadata for a rule pack or exchange file. Provides provenance,
/// versioning, and filtering capabilities for third-party distribution.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PackMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

/// Persistent overrides/pack file: a JSON object with schema version,
/// optional metadata, spelling overrides, and case overrides.
///
/// Used for both ~/.config/zhtw-mcp/overrides.json and pack files
/// in ~/.config/zhtw-mcp/packs/. The metadata is optional for
/// backward compatibility with plain override files.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Overrides {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<PackMetadata>,
    #[serde(default)]
    pub spelling: Vec<SpellingRule>,
    #[serde(default)]
    pub case: Vec<CaseRule>,
}

impl Default for Overrides {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            metadata: None,
            spelling: Vec::new(),
            case: Vec::new(),
        }
    }
}

/// The override store backed by a JSON file.
pub struct OverrideStore {
    path: PathBuf,
    overrides: Overrides,
}

impl OverrideStore {
    /// Open (or create) an override store at the given path.
    ///
    /// Recovery strategy: if the file exists but is corrupt JSON, back it up
    /// and start fresh rather than failing startup entirely.
    pub fn open(path: &Path) -> Result<Self> {
        let overrides = if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("read {}", path.display()))?;
            match serde_json::from_str::<Overrides>(&content) {
                Ok(ov) if ov.schema_version == SCHEMA_VERSION => ov,
                Ok(ov) => {
                    let ext = format!("v{}.bak", ov.schema_version);
                    tracing::warn!(
                        "override schema version mismatch (stored={}, expected={}); \
                         backing up and resetting",
                        ov.schema_version,
                        SCHEMA_VERSION,
                    );
                    backup_and_reset(path, &ext)
                }
                Err(e) => {
                    tracing::warn!("corrupt overrides JSON ({e}); backing up and resetting");
                    backup_and_reset(path, "corrupt.bak")
                }
            }
        } else {
            let ov = Overrides::default();
            atomic_write_json(path, &ov)?;
            ov
        };
        Ok(Self {
            path: path.to_owned(),
            overrides,
        })
    }

    /// Persist the given overrides to the JSON file atomically.
    fn flush_pending(&self, overrides: &Overrides) -> Result<()> {
        atomic_write_json(&self.path, overrides)
    }

    /// The path to the override file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load spelling rules: embedded base rules merged with overrides.
    pub fn load_spelling_rules(&self, base: &[SpellingRule]) -> Vec<SpellingRule> {
        merge_spelling_rules(&[base, &self.overrides.spelling])
    }

    /// Load case rules: embedded base rules merged with overrides.
    pub fn load_case_rules(&self, base: &[CaseRule]) -> Vec<CaseRule> {
        merge_case_rules(&[base, &self.overrides.case])
    }

    /// Upsert a spelling rule override.
    ///
    /// Persists atomically: in-memory state only updates after flush succeeds.
    pub fn upsert_spelling_override(&mut self, rule: &SpellingRule) -> Result<()> {
        let _lock = acquire_lock(&self.path)?;
        let mut pending = self.overrides.clone();
        if let Some(pos) = pending.spelling.iter().position(|r| r.from == rule.from) {
            pending.spelling[pos] = rule.clone();
        } else {
            pending.spelling.push(rule.clone());
        }
        self.flush_pending(&pending)?;
        self.overrides = pending;
        Ok(())
    }

    /// Upsert a case rule override.
    pub fn upsert_case_override(&mut self, rule: &CaseRule) -> Result<()> {
        let _lock = acquire_lock(&self.path)?;
        let lower = rule.term.to_lowercase();
        let mut pending = self.overrides.clone();
        if let Some(pos) = pending
            .case
            .iter()
            .position(|r| r.term.to_lowercase() == lower)
        {
            pending.case[pos] = rule.clone();
        } else {
            pending.case.push(rule.clone());
        }
        self.flush_pending(&pending)?;
        self.overrides = pending;
        Ok(())
    }

    /// Disable a spelling rule by writing a disabled override.
    pub fn disable_spelling_rule(&mut self, from_key: &str) -> Result<()> {
        let rule = SpellingRule {
            from: from_key.into(),
            to: vec![],
            rule_type: super::ruleset::RuleType::CrossStrait,

            disabled: true,
            context: None,
            english: None,
            exceptions: None,
            context_clues: None,
            negative_context_clues: None,
            positional_clues: None,
            tags: None,
            editorial_confidence: None,
        };
        self.upsert_spelling_override(&rule)
    }

    /// Disable a case rule by writing a disabled override.
    pub fn disable_case_rule(&mut self, term: &str) -> Result<()> {
        let rule = CaseRule {
            term: term.into(),
            alternatives: None,
            disabled: true,
        };
        self.upsert_case_override(&rule)
    }

    /// Delete a spelling rule override. Returns true if it existed.
    pub fn delete_spelling_override(&mut self, from_key: &str) -> Result<bool> {
        let _lock = acquire_lock(&self.path)?;
        let mut pending = self.overrides.clone();
        let before = pending.spelling.len();
        pending.spelling.retain(|r| r.from != from_key);
        let existed = pending.spelling.len() < before;
        if existed {
            self.flush_pending(&pending)?;
            self.overrides = pending;
        }
        Ok(existed)
    }

    /// Delete a case rule override. Returns true if it existed.
    pub fn delete_case_override(&mut self, term: &str) -> Result<bool> {
        let _lock = acquire_lock(&self.path)?;
        let lower = term.to_lowercase();
        let mut pending = self.overrides.clone();
        let before = pending.case.len();
        pending.case.retain(|r| r.term.to_lowercase() != lower);
        let existed = pending.case.len() < before;
        if existed {
            self.flush_pending(&pending)?;
            self.overrides = pending;
        }
        Ok(existed)
    }

    /// Clear all overrides.
    pub fn clear_overrides(&mut self) -> Result<()> {
        let _lock = acquire_lock(&self.path)?;
        let pending = Overrides::default();
        self.flush_pending(&pending)?;
        self.overrides = pending;
        Ok(())
    }

    /// Re-read overrides from disk. Used by zh_reload so that external
    /// edits to the overrides file are picked up without restarting the
    /// server.
    pub fn reload(&mut self) -> Result<()> {
        let refreshed = Self::open(&self.path)?;
        self.overrides = refreshed.overrides;
        Ok(())
    }

    /// Return user spelling overrides (not the merged/embedded ruleset).
    pub fn spelling_overrides(&self) -> &[SpellingRule] {
        &self.overrides.spelling
    }

    /// Return user case overrides (not the merged/embedded ruleset).
    pub fn case_overrides(&self) -> &[CaseRule] {
        &self.overrides.case
    }
}

/// Atomically write a serializable value to a JSON file.
///
/// Writes to a temporary file in the same directory, then renames over
/// the target. This prevents corruption on crash/power-loss.
fn atomic_write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let raw_parent = path.parent().unwrap_or_else(|| Path::new("."));
    // Normalize empty parent (bare filename like "overrides.json") to "."
    // so that NamedTempFile::new_in gets a valid directory.
    let parent = if raw_parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        raw_parent
    };
    std::fs::create_dir_all(parent).with_context(|| format!("create dir {}", parent.display()))?;
    let json = serde_json::to_string_pretty(value).context("serialize JSON")?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temp file in {}", parent.display()))?;
    tmp.write_all(json.as_bytes()).context("write temp JSON")?;
    tmp.persist(path)
        .with_context(|| format!("persist {}", path.display()))?;
    Ok(())
}

/// Best-effort rename of path to path.with_extension(ext).
/// Silently ignored on failure because the important thing is to not fail startup.
fn backup_file(path: &Path, ext: &str) {
    let backup = path.with_extension(ext);
    let _ = std::fs::rename(path, &backup);
}

/// Back up the file at path and return a fresh default Overrides.
fn backup_and_reset(path: &Path, ext: &str) -> Overrides {
    backup_file(path, ext);
    Overrides::default()
}

/// Resolve the default overrides file path.
///
/// Priority:
///   1. $XDG_CONFIG_HOME/zhtw-mcp/overrides.json (if absolute)
///   2. Platform-native config dir/zhtw-mcp/overrides.json
///   3. ./overrides.json (fallback)
pub fn default_overrides_path() -> PathBuf {
    config_dir()
        .map(|d| d.join("zhtw-mcp").join("overrides.json"))
        .unwrap_or_else(|| PathBuf::from("overrides.json"))
}

pub(crate) fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(dirs::config_dir)
}

// SuppressionStore: per-user term suppression list

/// Persistent suppression list. Suppressed terms are still scanned but their
/// severity is downgraded to Info in tool output.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Suppressions {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub terms: Vec<String>,
}

impl Default for Suppressions {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            terms: Vec::new(),
        }
    }
}

/// JSON-file-backed suppression store.
pub struct SuppressionStore {
    path: PathBuf,
    suppressions: Suppressions,
    /// O(1) lookup index mirroring suppressions.terms.
    term_set: HashSet<String>,
}

impl SuppressionStore {
    /// Open (or create) a suppression store at the given path.
    ///
    /// If the file exists but is corrupt or has a schema mismatch, it is
    /// backed up to <path>.bak before starting fresh (mirroring
    /// OverrideStore behavior).
    pub fn open(path: &Path) -> Result<Self> {
        let mut suppressions = if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("read {}", path.display()))?;
            match serde_json::from_str::<Suppressions>(&content) {
                Ok(s) if s.schema_version == SCHEMA_VERSION => s,
                Ok(s) => {
                    tracing::warn!(
                        "suppression schema mismatch (stored={}, expected={}); \
                         backing up and resetting",
                        s.schema_version,
                        SCHEMA_VERSION
                    );
                    backup_file(path, &format!("v{}.bak", s.schema_version));
                    Suppressions::default()
                }
                Err(e) => {
                    tracing::warn!("corrupt suppressions JSON ({e}); backing up and resetting");
                    backup_file(path, "corrupt.bak");
                    Suppressions::default()
                }
            }
        } else {
            let s = Suppressions::default();
            atomic_write_json(path, &s)?;
            s
        };
        // Build lookup set and enforce uniqueness invariant: if the JSON
        // file was hand-edited to contain duplicates, deduplicate so that
        // remove() keeps Vec and HashSet in sync.
        let mut term_set = HashSet::with_capacity(suppressions.terms.len());
        suppressions.terms.retain(|t| term_set.insert(t.clone()));
        Ok(Self {
            path: path.to_owned(),
            suppressions,
            term_set,
        })
    }

    fn flush(&self) -> Result<()> {
        atomic_write_json(&self.path, &self.suppressions)
    }

    /// Add a term to the suppression list. Returns false if already present.
    ///
    /// The in-memory state is mutated first, then flushed to disk. On flush
    /// failure the mutation is rolled back so memory stays consistent with
    /// disk.
    pub fn add(&mut self, term: &str) -> Result<bool> {
        let _lock = acquire_lock(&self.path)?;
        if self.term_set.contains(term) {
            return Ok(false);
        }
        self.suppressions.terms.push(term.to_string());
        self.term_set.insert(term.to_string());
        if let Err(e) = self.flush() {
            // Rollback in-memory change.
            self.suppressions.terms.pop();
            self.term_set.remove(term);
            return Err(e);
        }
        Ok(true)
    }

    /// Remove a term from the suppression list. Returns true if it existed.
    pub fn remove(&mut self, term: &str) -> Result<bool> {
        let _lock = acquire_lock(&self.path)?;
        let pos = self.suppressions.terms.iter().position(|t| t == term);
        let Some(idx) = pos else {
            return Ok(false);
        };
        let removed = self.suppressions.terms.remove(idx);
        self.term_set.remove(term);
        if let Err(e) = self.flush() {
            // Rollback: re-insert at original position.
            self.term_set.insert(removed.clone());
            self.suppressions.terms.insert(idx, removed);
            return Err(e);
        }
        Ok(true)
    }

    /// List all suppressed terms.
    pub fn list(&self) -> &[String] {
        &self.suppressions.terms
    }

    /// Clear all suppressions.
    pub fn clear(&mut self) -> Result<()> {
        let _lock = acquire_lock(&self.path)?;
        let old = std::mem::take(&mut self.suppressions);
        let old_set = std::mem::take(&mut self.term_set);
        self.suppressions = Suppressions::default();
        if let Err(e) = self.flush() {
            // Rollback.
            self.suppressions = old;
            self.term_set = old_set;
            return Err(e);
        }
        Ok(())
    }

    /// Check if a term is suppressed (O(1) via HashSet).
    pub fn is_suppressed(&self, term: &str) -> bool {
        self.term_set.contains(term)
    }

    /// The path to the suppressions file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Resolve the default suppressions file path.
pub fn default_suppressions_path() -> PathBuf {
    config_dir()
        .map(|d| d.join("zhtw-mcp").join("suppressions.json"))
        .unwrap_or_else(|| PathBuf::from("suppressions.json"))
}

// TranslationMemoryStore: persistent correction tracking

/// Schema version for .zhtw-tm.json. Independent of SCHEMA_VERSION since
/// the TM file evolves on its own timeline.
pub const TM_SCHEMA_VERSION: u32 = 1;

/// Maximum number of TM entries. Human-authored decisions; 10 000 is
/// generous for any realistic project. Prevents unbounded memory growth
/// from accidental or adversarial import.
const TM_MAX_ENTRIES: usize = 10_000;

/// A single correction record in the translation memory.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TmEntry {
    pub found: String,
    pub scanner_suggested: String,
    pub user_chose: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub timestamp: String,
}

/// On-disk format for `.zhtw-tm.json`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TranslationMemory {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub entries: Vec<TmEntry>,
}

impl Default for TranslationMemory {
    fn default() -> Self {
        Self {
            schema_version: TM_SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }
}

/// JSON-file-backed translation memory store.
///
/// Records user decisions about scanner suggestions for future scans.
/// When a user rejects a scanner suggestion (keeps the flagged term),
/// future scans suppress that rule for matching context. When the user
/// accepts, confidence is boosted.
pub struct TranslationMemoryStore {
    path: PathBuf,
    memory: TranslationMemory,
    /// Lookup index: `found` term -> index of the canonical entry in
    /// `memory.entries`. For duplicate `found` keys (hand-edited files),
    /// the index always points to the last occurrence.
    index: HashMap<String, usize>,
}

impl TranslationMemoryStore {
    /// Open (or create) a TM store at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        let memory = if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("read {}", path.display()))?;
            match serde_json::from_str::<TranslationMemory>(&content) {
                Ok(tm) if tm.schema_version == TM_SCHEMA_VERSION => tm,
                Ok(tm) => {
                    tracing::warn!(
                        "TM schema version mismatch (stored={}, expected={}); \
                         backing up and resetting",
                        tm.schema_version,
                        TM_SCHEMA_VERSION,
                    );
                    backup_file(path, &format!("v{}.bak", tm.schema_version));
                    TranslationMemory::default()
                }
                Err(e) => {
                    tracing::warn!("corrupt TM JSON ({e}); backing up and resetting");
                    backup_file(path, "corrupt.bak");
                    TranslationMemory::default()
                }
            }
        } else {
            TranslationMemory::default()
        };
        let index = build_tm_index(&memory.entries);
        Ok(Self {
            path: path.to_owned(),
            memory,
            index,
        })
    }

    fn flush(&self) -> Result<()> {
        atomic_write_json(&self.path, &self.memory)
    }

    /// Record a correction decision. Deduplicates by `found`: the most recent
    /// decision for a term always overwrites the previous one, regardless of
    /// context. This ensures a user can undo a rejection by accepting later.
    pub fn record(&mut self, entry: TmEntry) -> Result<()> {
        let _lock = acquire_lock(&self.path)?;

        // Deduplicate by found: latest decision wins. O(1) via index.
        let existing = self.index.get(&entry.found).copied();

        // Enforce entry cap (new entries only; updates always allowed).
        if existing.is_none() && self.memory.entries.len() >= TM_MAX_ENTRIES {
            anyhow::bail!(
                "translation memory full ({TM_MAX_ENTRIES} entries); \
                 clear or export before recording more"
            );
        }

        // Save old entry for rollback before mutating.
        let old_entry = existing.map(|pos| self.memory.entries[pos].clone());

        if let Some(pos) = existing {
            self.memory.entries[pos] = entry;
        } else {
            let new_idx = self.memory.entries.len();
            let found_key = entry.found.clone();
            self.memory.entries.push(entry);
            self.index.insert(found_key, new_idx);
        }

        if let Err(e) = self.flush() {
            // Rollback: restore previous state.
            if let Some(old) = old_entry {
                self.memory.entries[existing.unwrap()] = old;
            } else {
                let removed = self.memory.entries.pop().expect("just pushed");
                self.index.remove(&removed.found);
            }
            return Err(e);
        }
        Ok(())
    }

    /// Check if the user previously rejected the scanner suggestion for this
    /// term (i.e. user_chose == found on the latest TM entry for that term).
    ///
    /// Matches by `found` only. Uses the last index entry (latest decision)
    /// to avoid stale duplicates in hand-edited files poisoning the result.
    pub fn should_suppress(&self, found: &str) -> bool {
        self.index.get(found).is_some_and(|&idx| {
            let e = &self.memory.entries[idx];
            e.user_chose == e.found
        })
    }

    /// List all TM entries.
    pub fn list(&self) -> &[TmEntry] {
        &self.memory.entries
    }

    /// Clear all TM entries.
    pub fn clear(&mut self) -> Result<()> {
        let _lock = acquire_lock(&self.path)?;
        let old_memory = std::mem::take(&mut self.memory);
        let old_index = std::mem::take(&mut self.index);
        self.memory = TranslationMemory::default();
        if let Err(e) = self.flush() {
            self.memory = old_memory;
            self.index = old_index;
            return Err(e);
        }
        Ok(())
    }

    /// Export TM entries to a file.
    pub fn export(&self, dest: &Path) -> Result<()> {
        atomic_write_json(dest, &self.memory)
    }

    /// Import TM entries from a file. Merges with existing entries (dedup
    /// by `found` only — latest decision per term wins). Returns `(added, updated)`.
    pub fn import(&mut self, src: &Path) -> Result<(usize, usize)> {
        let _lock = acquire_lock(&self.path)?;
        let content =
            std::fs::read_to_string(src).with_context(|| format!("read {}", src.display()))?;
        let imported: TranslationMemory =
            serde_json::from_str(&content).context("invalid TM JSON")?;

        // Snapshot for rollback.
        let old_entries = self.memory.entries.clone();
        let old_index = self.index.clone();

        let mut added = 0;
        let mut updated = 0;
        for entry in imported.entries {
            if let Some(&pos) = self.index.get(&entry.found) {
                // Latest-wins: update existing entry in place.
                self.memory.entries[pos] = entry;
                updated += 1;
            } else {
                if self.memory.entries.len() >= TM_MAX_ENTRIES {
                    tracing::warn!(
                        "TM entry cap ({TM_MAX_ENTRIES}) reached during import; \
                         skipping new entries (updates still applied)"
                    );
                    continue;
                }
                let new_idx = self.memory.entries.len();
                self.index.insert(entry.found.clone(), new_idx);
                self.memory.entries.push(entry);
                added += 1;
            }
        }

        if added > 0 || updated > 0 {
            if let Err(e) = self.flush() {
                self.memory.entries = old_entries;
                self.index = old_index;
                return Err(e);
            }
        }
        Ok((added, updated))
    }

    /// Re-read TM from disk.
    pub fn reload(&mut self) -> Result<()> {
        let refreshed = Self::open(&self.path)?;
        self.memory = refreshed.memory;
        self.index = refreshed.index;
        Ok(())
    }

    /// The path to the TM file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Build lookup index from TM entries. For duplicate `found` keys
/// (hand-edited files), the last occurrence wins — matching the
/// 'latest decision wins' semantics.
fn build_tm_index(entries: &[TmEntry]) -> HashMap<String, usize> {
    let mut index: HashMap<String, usize> = HashMap::with_capacity(entries.len());
    for (idx, entry) in entries.iter().enumerate() {
        index.insert(entry.found.clone(), idx);
    }
    index
}

/// Discover `.zhtw-tm.json` by walking from start_dir upward to .git root.
/// Returns the path where the TM file should live (may not exist yet).
pub fn discover_tm_path(start_dir: &Path) -> PathBuf {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join(".zhtw-tm.json");
        if candidate.is_file() {
            return candidate;
        }
        // At .git root: use this directory for TM even if file does not exist yet.
        if dir.join(".git").exists() {
            return dir.join(".zhtw-tm.json");
        }
        if !dir.pop() {
            // No VCS root found; use cwd.
            return start_dir.join(".zhtw-tm.json");
        }
    }
}

/// Return today's date as an ISO 8601 string (YYYY-MM-DD, UTC).
/// No external dependency; civil calendar arithmetic from Unix epoch.
pub fn iso_date_today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// PackStore: domain-specific rule packs

/// Resolve the default packs directory.
pub fn default_packs_dir() -> PathBuf {
    config_dir()
        .map(|d| d.join("zhtw-mcp").join("packs"))
        .unwrap_or_else(|| PathBuf::from("packs"))
}

/// Summary of a single pack for listing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PackInfo {
    pub name: String,
    pub spelling_count: usize,
    pub case_count: usize,
    pub metadata: Option<PackMetadata>,
}

/// Conflict when two sources define the same from with different to.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PackConflict {
    pub from: String,
    pub source_a: String,
    pub to_a: Vec<String>,
    pub source_b: String,
    pub to_b: Vec<String>,
}

/// Manages the packs directory. Each pack is a JSON file with the same
/// schema as Overrides.
pub struct PackStore {
    dir: PathBuf,
}

impl PackStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// List all available packs (sorted by filename).
    pub fn list(&self) -> Vec<PackInfo> {
        let mut packs = Vec::new();
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(_) => return packs,
        };
        let mut files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .collect();
        files.sort_by_key(|e| e.file_name());

        for entry in files {
            let name = entry
                .path()
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if let Ok(ov) = self.load(&name) {
                packs.push(PackInfo {
                    name,
                    spelling_count: ov.spelling.len(),
                    case_count: ov.case.len(),
                    metadata: ov.metadata.clone(),
                });
            }
        }
        packs
    }

    /// Load a pack by name.
    pub fn load(&self, name: &str) -> Result<Overrides> {
        Self::validate_pack_name(name)?;
        let path = self.dir.join(format!("{name}.json"));
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("read pack: {}", path.display()))?;
        serde_json::from_str(&content).with_context(|| format!("parse pack: {}", path.display()))
    }

    /// Install a pack from a source file. Validates JSON before writing.
    pub fn install(&self, name: &str, source: &Path) -> Result<()> {
        Self::validate_pack_name(name)?;
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("create packs dir: {}", self.dir.display()))?;
        let content = std::fs::read_to_string(source)
            .with_context(|| format!("read pack source: {}", source.display()))?;
        let _ov: Overrides = serde_json::from_str(&content).context("invalid pack JSON schema")?;
        let dest = self.dir.join(format!("{name}.json"));
        atomic_write_json(&dest, &_ov)
    }

    /// Reject pack names that could escape the packs directory (path traversal)
    /// or cause filesystem issues (Windows reserved names, trailing dots/spaces).
    fn validate_pack_name(name: &str) -> Result<()> {
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name.contains("..")
            || name.contains('\0')
            || name == "."
        {
            anyhow::bail!("invalid pack name: '{name}' (must not contain path separators, null bytes, or '..')");
        }

        if name.ends_with('.') || name.ends_with(' ') {
            anyhow::bail!("invalid pack name: '{name}' (must not end with dot or space)");
        }

        // Windows reserved names (case-insensitive).
        let upper = name.to_ascii_uppercase();
        match upper.as_str() {
            "CON" | "PRN" | "AUX" | "NUL" | "COM1" | "COM2" | "COM3" | "COM4" | "COM5" | "COM6"
            | "COM7" | "COM8" | "COM9" | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5" | "LPT6"
            | "LPT7" | "LPT8" | "LPT9" => {
                anyhow::bail!("invalid pack name: '{name}' (reserved system name)");
            }
            _ => {}
        }

        Ok(())
    }

    /// Export a pack to a file.
    pub fn export(&self, name: &str, dest: &Path) -> Result<()> {
        let ov = self.load(name)?;
        atomic_write_json(dest, &ov)
    }

    /// Validate a pack file: schema, deduplication, reference integrity.
    pub fn validate(path: &Path) -> Result<Vec<String>> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("read: {}", path.display()))?;
        let ov: Overrides = serde_json::from_str(&content).context("invalid JSON schema")?;

        let mut warnings = Vec::new();

        // Check for duplicate from keys.
        let mut seen = HashSet::new();
        for rule in &ov.spelling {
            if !seen.insert(&rule.from) {
                warnings.push(format!("duplicate from key: {}", rule.from));
            }
        }

        // Check @seealso references.
        let from_set: HashSet<&str> = ov.spelling.iter().map(|r| r.from.as_str()).collect();
        for rule in &ov.spelling {
            if let Some(ctx) = &rule.context {
                for cap in ctx.match_indices("(@seealso") {
                    let start = cap.0 + "(@seealso".len();
                    if let Some(end) = ctx[start..].find(')') {
                        let refs_str = &ctx[start..start + end];
                        for r in refs_str.split(',') {
                            let r = r.trim();
                            if !r.is_empty() && !from_set.contains(r) {
                                warnings.push(format!(
                                    "rule '{}': @seealso references unknown '{}'",
                                    rule.from, r
                                ));
                            }
                        }
                    }
                }
            }
        }

        Ok(warnings)
    }

    /// The packs directory path.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Detect conflicts between named packs. Two packs conflict when they
/// define the same from key with different to values.
pub fn detect_pack_conflicts(packs: &[(String, &Overrides)]) -> Vec<PackConflict> {
    use std::collections::HashMap;
    let mut seen: HashMap<&str, (usize, &[String])> = HashMap::new();
    let mut conflicts = Vec::new();

    for (idx, (name, ov)) in packs.iter().enumerate() {
        for rule in &ov.spelling {
            if let Some(&(prev_idx, prev_to)) = seen.get(rule.from.as_str()) {
                if prev_to != rule.to.as_slice() {
                    conflicts.push(PackConflict {
                        from: rule.from.clone(),
                        source_a: packs[prev_idx].0.clone(),
                        to_a: prev_to.to_vec(),
                        source_b: name.clone(),
                        to_b: rule.to.clone(),
                    });
                }
            }
            seen.insert(&rule.from, (idx, &rule.to));
        }
    }
    conflicts
}

/// Build merged spelling and case rules from a base ruleset, override store,
/// and active packs. Encapsulates the load-layer-merge pipeline shared by
/// both the MCP server and the CLI batch linter.
pub fn build_merged_rules(
    base_spelling: &[SpellingRule],
    base_case: &[CaseRule],
    store: &OverrideStore,
    pack_store: &PackStore,
    active_packs: &[String],
) -> (Vec<SpellingRule>, Vec<CaseRule>) {
    let mut spelling_layers: Vec<Vec<SpellingRule>> =
        vec![store.load_spelling_rules(base_spelling)];
    let mut case_layers: Vec<Vec<CaseRule>> = vec![store.load_case_rules(base_case)];

    for pack_name in active_packs {
        match pack_store.load(pack_name) {
            Ok(pack) => {
                spelling_layers.push(pack.spelling);
                case_layers.push(pack.case);
            }
            Err(e) => {
                tracing::warn!("failed to load pack '{}': {}", pack_name, e);
            }
        }
    }

    let spelling_refs: Vec<&[SpellingRule]> =
        spelling_layers.iter().map(|v| v.as_slice()).collect();
    let case_refs: Vec<&[CaseRule]> = case_layers.iter().map(|v| v.as_slice()).collect();

    (
        merge_spelling_rules(&spelling_refs),
        merge_case_rules(&case_refs),
    )
}

/// Merge spelling rules from multiple layers. Later layers override earlier
/// ones (same from key). Disabled rules are removed.
///
/// Canonical merge order: base embedded ruleset -> overrides.json -> packs
/// in lexicographic filename order.
pub fn merge_spelling_rules(layers: &[&[SpellingRule]]) -> Vec<SpellingRule> {
    let total: usize = layers.iter().map(|l| l.len()).sum();
    let mut index: HashMap<String, usize> = HashMap::with_capacity(total);
    let mut rules: Vec<SpellingRule> = Vec::with_capacity(total);

    for layer in layers {
        for rule in *layer {
            if let Some(&pos) = index.get(rule.from.as_str()) {
                rules[pos] = rule.clone();
            } else {
                index.insert(rule.from.clone(), rules.len());
                rules.push(rule.clone());
            }
        }
    }
    rules.retain(|r| !r.disabled);
    rules
}

/// Merge case rules from multiple layers. Later layers override earlier
/// ones (case-insensitive term match). Disabled rules are removed.
pub fn merge_case_rules(layers: &[&[CaseRule]]) -> Vec<CaseRule> {
    let total: usize = layers.iter().map(|l| l.len()).sum();
    let mut index: HashMap<String, usize> = HashMap::with_capacity(total);
    let mut rules: Vec<CaseRule> = Vec::with_capacity(total);

    for layer in layers {
        for rule in *layer {
            let lower = rule.term.to_lowercase();
            if let Some(&pos) = index.get(lower.as_str()) {
                rules[pos] = rule.clone();
            } else {
                index.insert(lower, rules.len());
                rules.push(rule.clone());
            }
        }
    }
    rules.retain(|r| !r.disabled);
    rules
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ruleset::RuleType;

    fn sample_base_spelling() -> Vec<SpellingRule> {
        vec![
            SpellingRule {
                from: "軟件".into(),
                to: vec!["軟體".into()],
                rule_type: RuleType::CrossStrait,

                disabled: false,
                context: None,
                english: None,
                exceptions: None,
                context_clues: None,
                negative_context_clues: None,
                positional_clues: None,
                tags: None,
                editorial_confidence: None,
            },
            SpellingRule {
                from: "內存".into(),
                to: vec!["記憶體".into()],
                rule_type: RuleType::CrossStrait,

                disabled: false,
                context: None,
                english: None,
                exceptions: None,
                context_clues: None,
                negative_context_clues: None,
                positional_clues: None,
                tags: None,
                editorial_confidence: None,
            },
        ]
    }

    fn sample_base_case() -> Vec<CaseRule> {
        vec![CaseRule {
            term: "JavaScript".into(),
            alternatives: Some(vec!["javascript".into()]),
            disabled: false,
        }]
    }

    #[test]
    fn load_base_rules_without_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overrides.json");
        let store = OverrideStore::open(&path).unwrap();

        let spelling = store.load_spelling_rules(&sample_base_spelling());
        assert_eq!(spelling.len(), 2);
        assert_eq!(spelling[0].from, "軟件");

        let case = store.load_case_rules(&sample_base_case());
        assert_eq!(case.len(), 1);
        assert_eq!(case[0].term, "JavaScript");
    }

    #[test]
    fn spelling_override_upsert_and_merge() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overrides.json");
        let mut store = OverrideStore::open(&path).unwrap();

        // Override existing rule.
        let override_rule = SpellingRule {
            from: "軟件".into(),
            to: vec!["軟體".into(), "應用程式".into()],
            rule_type: RuleType::CrossStrait,

            disabled: false,
            context: None,
            english: None,
            exceptions: None,
            context_clues: None,
            negative_context_clues: None,
            positional_clues: None,
            tags: None,
            editorial_confidence: None,
        };
        store.upsert_spelling_override(&override_rule).unwrap();

        let rules = store.load_spelling_rules(&sample_base_spelling());
        assert_eq!(rules.len(), 2);
        let r = rules.iter().find(|r| r.from == "軟件").unwrap();
        assert_eq!(r.to.len(), 2);

        // Add new override.
        let new_rule = SpellingRule {
            from: "視頻".into(),
            to: vec!["影片".into()],
            rule_type: RuleType::CrossStrait,

            disabled: false,
            context: None,
            english: None,
            exceptions: None,
            context_clues: None,
            negative_context_clues: None,
            positional_clues: None,
            tags: None,
            editorial_confidence: None,
        };
        store.upsert_spelling_override(&new_rule).unwrap();

        let rules = store.load_spelling_rules(&sample_base_spelling());
        assert_eq!(rules.len(), 3);
    }

    #[test]
    fn disable_builtin_spelling_rule() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overrides.json");
        let mut store = OverrideStore::open(&path).unwrap();
        let base = sample_base_spelling();

        assert_eq!(store.load_spelling_rules(&base).len(), 2);

        store.disable_spelling_rule("軟件").unwrap();
        let rules = store.load_spelling_rules(&base);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].from, "內存");

        // Re-enable by deleting the override.
        store.delete_spelling_override("軟件").unwrap();
        assert_eq!(store.load_spelling_rules(&base).len(), 2);
    }

    #[test]
    fn disable_builtin_case_rule() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overrides.json");
        let mut store = OverrideStore::open(&path).unwrap();
        let base = sample_base_case();

        assert_eq!(store.load_case_rules(&base).len(), 1);

        store.disable_case_rule("JavaScript").unwrap();
        assert_eq!(store.load_case_rules(&base).len(), 0);

        store.delete_case_override("JavaScript").unwrap();
        assert_eq!(store.load_case_rules(&base).len(), 1);
    }

    #[test]
    fn case_override_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overrides.json");
        let mut store = OverrideStore::open(&path).unwrap();
        let base = sample_base_case();

        let new_case = CaseRule {
            term: "TypeScript".into(),
            alternatives: None,
            disabled: false,
        };
        store.upsert_case_override(&new_case).unwrap();
        assert_eq!(store.load_case_rules(&base).len(), 2);

        let deleted = store.delete_case_override("TypeScript").unwrap();
        assert!(deleted);
        assert_eq!(store.load_case_rules(&base).len(), 1);
    }

    #[test]
    fn overrides_persist_across_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overrides.json");

        // Write an override.
        {
            let mut store = OverrideStore::open(&path).unwrap();
            let rule = SpellingRule {
                from: "視頻".into(),
                to: vec!["影片".into()],
                rule_type: RuleType::CrossStrait,

                disabled: false,
                context: None,
                english: None,
                exceptions: None,
                context_clues: None,
                negative_context_clues: None,
                positional_clues: None,
                tags: None,
                editorial_confidence: None,
            };
            store.upsert_spelling_override(&rule).unwrap();
        }

        // Re-open and verify.
        let store = OverrideStore::open(&path).unwrap();
        let base = sample_base_spelling();
        let rules = store.load_spelling_rules(&base);
        assert_eq!(rules.len(), 3);
        assert!(rules.iter().any(|r| r.from == "視頻"));
    }

    #[test]
    fn schema_version_mismatch_resets_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overrides.json");

        // Write overrides with wrong schema version.
        let bad = Overrides {
            schema_version: 1,
            metadata: None,
            spelling: vec![SpellingRule {
                from: "test".into(),
                to: vec!["ok".into()],
                rule_type: RuleType::CrossStrait,

                disabled: false,
                context: None,
                english: None,
                exceptions: None,
                context_clues: None,
                negative_context_clues: None,
                positional_clues: None,
                tags: None,
                editorial_confidence: None,
            }],
            case: vec![],
        };
        std::fs::write(&path, serde_json::to_string(&bad).unwrap()).unwrap();

        let store = OverrideStore::open(&path).unwrap();
        // Should have reset to empty.
        assert!(store.overrides.spelling.is_empty());
        assert_eq!(store.overrides.schema_version, SCHEMA_VERSION);

        // Old file should be backed up.
        let backup = dir.path().join("overrides.v1.bak");
        assert!(backup.exists(), "backup file should exist");
    }

    #[test]
    fn corrupt_json_resets_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overrides.json");

        std::fs::write(&path, "{ this is not valid json }").unwrap();

        let store = OverrideStore::open(&path).unwrap();
        assert!(store.overrides.spelling.is_empty());
        assert_eq!(store.overrides.schema_version, SCHEMA_VERSION);

        let backup = dir.path().join("overrides.corrupt.bak");
        assert!(backup.exists(), "corrupt backup should exist");
    }

    #[test]
    fn clear_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overrides.json");
        let mut store = OverrideStore::open(&path).unwrap();

        store.disable_spelling_rule("test").unwrap();
        assert!(!store.overrides.spelling.is_empty());

        store.clear_overrides().unwrap();
        assert!(store.overrides.spelling.is_empty());
        assert!(store.overrides.case.is_empty());
    }

    // SuppressionStore tests

    #[test]
    fn suppression_add_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("suppressions.json");
        let mut store = SuppressionStore::open(&path).unwrap();

        assert!(store.list().is_empty());

        assert!(store.add("軟件").unwrap());
        assert!(!store.add("軟件").unwrap()); // duplicate
        assert!(store.is_suppressed("軟件"));
        assert!(!store.is_suppressed("硬件"));
        assert_eq!(store.list().len(), 1);

        assert!(store.remove("軟件").unwrap());
        assert!(!store.remove("軟件").unwrap()); // already removed
        assert!(!store.is_suppressed("軟件"));
    }

    #[test]
    fn suppression_persists_across_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("suppressions.json");

        {
            let mut store = SuppressionStore::open(&path).unwrap();
            store.add("信息").unwrap();
            store.add("網絡").unwrap();
        }

        let store = SuppressionStore::open(&path).unwrap();
        assert_eq!(store.list().len(), 2);
        assert!(store.is_suppressed("信息"));
        assert!(store.is_suppressed("網絡"));
    }

    #[test]
    fn suppression_clear() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("suppressions.json");
        let mut store = SuppressionStore::open(&path).unwrap();

        store.add("test1").unwrap();
        store.add("test2").unwrap();
        assert_eq!(store.list().len(), 2);

        store.clear().unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn suppression_default_file_created_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("suppressions.json");
        let _store = SuppressionStore::open(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn suppression_deduplicates_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("suppressions.json");

        // Simulate a hand-edited file with duplicate terms.
        let duped = Suppressions {
            schema_version: SCHEMA_VERSION,
            terms: vec!["軟件".into(), "軟件".into(), "網絡".into()],
        };
        std::fs::write(&path, serde_json::to_string(&duped).unwrap()).unwrap();

        let mut store = SuppressionStore::open(&path).unwrap();
        assert_eq!(store.list().len(), 2);
        assert!(store.is_suppressed("軟件"));

        // After removing, both Vec and HashSet should agree.
        assert!(store.remove("軟件").unwrap());
        assert!(!store.is_suppressed("軟件"));
        assert_eq!(store.list().len(), 1);
    }

    // PackStore name validation tests

    #[test]
    fn pack_name_rejects_path_traversal() {
        assert!(PackStore::validate_pack_name("../evil").is_err());
        assert!(PackStore::validate_pack_name("foo/bar").is_err());
        assert!(PackStore::validate_pack_name("foo\\bar").is_err());
        assert!(PackStore::validate_pack_name("..").is_err());
        assert!(PackStore::validate_pack_name(".").is_err());
        assert!(PackStore::validate_pack_name("").is_err());
    }

    #[test]
    fn pack_name_accepts_valid_names() {
        assert!(PackStore::validate_pack_name("medical").is_ok());
        assert!(PackStore::validate_pack_name("it-terms").is_ok());
        assert!(PackStore::validate_pack_name("my_pack.v2").is_ok());
    }

    // TranslationMemoryStore tests

    #[test]
    fn tm_record_and_suppress() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".zhtw-tm.json");
        let mut store = TranslationMemoryStore::open(&path).unwrap();

        assert!(store.list().is_empty());

        // Record a rejection (user kept the flagged term).
        store
            .record(TmEntry {
                found: "線程".into(),
                scanner_suggested: "執行緒".into(),
                user_chose: "線程".into(),
                context: Some("作業系統".into()),
                timestamp: "2026-03-18".into(),
            })
            .unwrap();

        assert_eq!(store.list().len(), 1);
        // Rejection suppresses the term regardless of context.
        assert!(store.should_suppress("線程"));
        // Different term is not suppressed.
        assert!(!store.should_suppress("調用"));
    }

    #[test]
    fn tm_acceptance_does_not_suppress() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".zhtw-tm.json");
        let mut store = TranslationMemoryStore::open(&path).unwrap();

        // User accepted the scanner suggestion.
        store
            .record(TmEntry {
                found: "調用".into(),
                scanner_suggested: "呼叫".into(),
                user_chose: "呼叫".into(),
                context: None,
                timestamp: "2026-03-18".into(),
            })
            .unwrap();

        // Acceptance does not suppress (user_chose != found).
        assert!(!store.should_suppress("調用"));
    }

    #[test]
    fn tm_deduplicates_by_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".zhtw-tm.json");
        let mut store = TranslationMemoryStore::open(&path).unwrap();

        // Record a rejection with context.
        store
            .record(TmEntry {
                found: "線程".into(),
                scanner_suggested: "執行緒".into(),
                user_chose: "線程".into(),
                context: Some("OS".into()),
                timestamp: "2026-03-18".into(),
            })
            .unwrap();
        assert!(store.should_suppress("線程"));

        // Accept with different context: overwrites the rejection (dedup by found).
        store
            .record(TmEntry {
                found: "線程".into(),
                scanner_suggested: "執行緒".into(),
                user_chose: "執行緒".into(),
                context: None,
                timestamp: "2026-03-19".into(),
            })
            .unwrap();

        assert_eq!(store.list().len(), 1);
        assert_eq!(store.list()[0].user_chose, "執行緒");
        // Acceptance overwrote rejection: no longer suppresses.
        assert!(!store.should_suppress("線程"));
    }

    #[test]
    fn tm_persists_across_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".zhtw-tm.json");

        {
            let mut store = TranslationMemoryStore::open(&path).unwrap();
            store
                .record(TmEntry {
                    found: "信息".into(),
                    scanner_suggested: "資訊".into(),
                    user_chose: "信息".into(),
                    context: None,
                    timestamp: "2026-03-18".into(),
                })
                .unwrap();
        }

        let store = TranslationMemoryStore::open(&path).unwrap();
        assert_eq!(store.list().len(), 1);
        assert!(store.should_suppress("信息"));
    }

    #[test]
    fn tm_clear() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".zhtw-tm.json");
        let mut store = TranslationMemoryStore::open(&path).unwrap();

        store
            .record(TmEntry {
                found: "test".into(),
                scanner_suggested: "ok".into(),
                user_chose: "test".into(),
                context: None,
                timestamp: "2026-03-18".into(),
            })
            .unwrap();
        assert_eq!(store.list().len(), 1);

        store.clear().unwrap();
        assert!(store.list().is_empty());
        assert!(!store.should_suppress("test"));
    }

    #[test]
    fn tm_export_import_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("source.json");
        let dest_path = dir.path().join("dest.json");

        // Create source TM with entries.
        {
            let mut store = TranslationMemoryStore::open(&src_path).unwrap();
            store
                .record(TmEntry {
                    found: "線程".into(),
                    scanner_suggested: "執行緒".into(),
                    user_chose: "執行緒".into(),
                    context: None,
                    timestamp: "2026-03-18".into(),
                })
                .unwrap();
            store
                .record(TmEntry {
                    found: "內存".into(),
                    scanner_suggested: "記憶體".into(),
                    user_chose: "內存".into(),
                    context: Some("casual".into()),
                    timestamp: "2026-03-18".into(),
                })
                .unwrap();
            store.export(&dest_path).unwrap();
        }

        // Import into a fresh TM.
        let import_path = dir.path().join("target.json");
        let mut target = TranslationMemoryStore::open(&import_path).unwrap();
        let (added, updated) = target.import(&dest_path).unwrap();
        assert_eq!(added, 2);
        assert_eq!(updated, 0);
        assert_eq!(target.list().len(), 2);

        // Import again: updates existing, adds none.
        let (added2, updated2) = target.import(&dest_path).unwrap();
        assert_eq!(added2, 0);
        assert_eq!(updated2, 2);
        assert_eq!(target.list().len(), 2);
    }

    #[test]
    fn tm_schema_mismatch_resets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".zhtw-tm.json");

        let bad = TranslationMemory {
            schema_version: 999,
            entries: vec![TmEntry {
                found: "test".into(),
                scanner_suggested: "ok".into(),
                user_chose: "test".into(),
                context: None,
                timestamp: "2026-03-18".into(),
            }],
        };
        std::fs::write(&path, serde_json::to_string(&bad).unwrap()).unwrap();

        let store = TranslationMemoryStore::open(&path).unwrap();
        assert!(store.list().is_empty());

        let backup = dir.path().join(".zhtw-tm.v999.bak");
        assert!(backup.exists());
    }

    #[test]
    fn tm_discover_path_at_git_root() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let sub = repo.join("src").join("deep");
        std::fs::create_dir_all(&sub).unwrap();

        let discovered = discover_tm_path(&sub);
        assert_eq!(discovered, repo.join(".zhtw-tm.json"));
    }

    #[test]
    fn tm_hand_edited_duplicates_record_updates_last() {
        // Simulate a hand-edited TM file with duplicate found entries.
        // record() should update the last one (via index), and
        // should_suppress() should read the canonical one.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".zhtw-tm.json");

        let duped = TranslationMemory {
            schema_version: TM_SCHEMA_VERSION,
            entries: vec![
                TmEntry {
                    found: "線程".into(),
                    scanner_suggested: "執行緒".into(),
                    user_chose: "線程".into(), // rejection
                    context: None,
                    timestamp: "2026-03-01".into(),
                },
                TmEntry {
                    found: "線程".into(),
                    scanner_suggested: "執行緒".into(),
                    user_chose: "線程".into(), // also rejection (stale dup)
                    context: Some("old".into()),
                    timestamp: "2026-03-02".into(),
                },
            ],
        };
        std::fs::write(&path, serde_json::to_string(&duped).unwrap()).unwrap();

        let mut store = TranslationMemoryStore::open(&path).unwrap();
        assert!(store.should_suppress("線程")); // last entry is rejection

        // Now accept via record: should update the LAST entry.
        store
            .record(TmEntry {
                found: "線程".into(),
                scanner_suggested: "執行緒".into(),
                user_chose: "執行緒".into(), // acceptance
                context: None,
                timestamp: "2026-03-19".into(),
            })
            .unwrap();

        // should_suppress reads last entry, which is now acceptance.
        assert!(!store.should_suppress("線程"));
        // First (stale) entry is unchanged; record updated the last one.
        assert_eq!(store.list()[0].user_chose, "線程");
        assert_eq!(store.list()[1].user_chose, "執行緒");
    }
}
