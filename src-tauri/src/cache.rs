//! Incremental scan cache — per-provider on-disk state that lets the
//! scanner skip files whose (mtime, size) haven't changed since last run
//! and resume parsing at a saved byte offset for files that have grown.
//!
//! Schema is a straight port of the Swift `CostUsageCache` in the macOS
//! app (`CLIPulseCore/CostUsageCache.swift`), kept deliberately simple so
//! the two implementations stay interchangeable at the concept level
//! (not binary — this is a Rust-only v1 file living at its own path to
//! avoid cross-app interference).
//!
//! Cache location:
//!   macOS:   ~/Library/Caches/dev.clipulse.desktop/cost-usage/{provider}-v1.json
//!   Linux:   ~/.cache/dev.clipulse.desktop/cost-usage/{provider}-v1.json
//!   Windows: %LOCALAPPDATA%\dev.clipulse.desktop\cost-usage\{provider}-v1.json
//!
//! Packed slot layout (matches Swift):
//!   Codex:  [input, cached, output]
//!   Claude: [input, cache_read, cache_create, output, cost_nanos, msgs]
//!
//! `cost_nanos` is per-message cost accumulated during parse, stored at
//! a billion-scale (i.e. `$0.01 → 10_000_000`). Pre-aggregation of cost
//! is essential because Claude's tiered pricing must evaluate per-message,
//! not on the day's aggregated tokens (see scanner.rs commit history).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const CACHE_SCHEMA_VERSION: u32 = 1;

pub type Packed = Vec<i64>;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct CodexTotals {
    pub input: i64,
    pub cached: i64,
    pub output: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub mtime_unix_ms: i64,
    pub size: i64,
    /// This file's own contribution: `day -> model -> packed`. Kept per
    /// file (not only at cache-top) so we can subtract it when the file
    /// rotates / shrinks / disappears.
    pub days: HashMap<String, HashMap<String, Packed>>,
    #[serde(default)]
    pub parsed_bytes: Option<i64>,
    #[serde(default)]
    pub last_model: Option<String>,
    #[serde(default)]
    pub last_totals: Option<CodexTotals>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostUsageCache {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub last_scan_unix_ms: i64,
    #[serde(default)]
    pub files: HashMap<String, FileEntry>,
    /// Aggregate across all tracked files: `day -> model -> packed`.
    /// Scanner emits DailyEntries straight from this without walking
    /// `files` — much cheaper on warm scans.
    #[serde(default)]
    pub days: HashMap<String, HashMap<String, Packed>>,
}

fn default_version() -> u32 {
    CACHE_SCHEMA_VERSION
}

impl Default for CostUsageCache {
    fn default() -> Self {
        Self {
            version: CACHE_SCHEMA_VERSION,
            last_scan_unix_ms: 0,
            files: HashMap::new(),
            days: HashMap::new(),
        }
    }
}

// ========================================================================
// IO
// ========================================================================

fn cache_root(override_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(o) = override_dir {
        return Some(o.to_path_buf());
    }
    dirs::cache_dir().map(|d| d.join("dev.clipulse.desktop").join("cost-usage"))
}

pub fn cache_path(provider: &str, override_dir: Option<&Path>) -> Option<PathBuf> {
    cache_root(override_dir).map(|d| d.join(format!("{}-v{}.json", provider, CACHE_SCHEMA_VERSION)))
}

pub fn load(provider: &str, override_dir: Option<&Path>) -> CostUsageCache {
    let path = match cache_path(provider, override_dir) {
        Some(p) => p,
        None => return CostUsageCache::default(),
    };
    if !path.exists() {
        return CostUsageCache::default();
    }
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("cache::load read failed ({}): {e}", path.display());
            return CostUsageCache::default();
        }
    };
    match serde_json::from_str::<CostUsageCache>(&text) {
        Ok(cache) if cache.version == CACHE_SCHEMA_VERSION => cache,
        Ok(cache) => {
            log::info!(
                "cache::load version mismatch ({} vs {}), starting fresh",
                cache.version,
                CACHE_SCHEMA_VERSION
            );
            CostUsageCache::default()
        }
        Err(e) => {
            log::warn!(
                "cache::load parse failed ({}): {e} — starting fresh",
                path.display()
            );
            CostUsageCache::default()
        }
    }
}

pub fn save(
    provider: &str,
    cache: &CostUsageCache,
    override_dir: Option<&Path>,
) -> anyhow::Result<()> {
    let path = cache_path(provider, override_dir)
        .ok_or_else(|| anyhow::anyhow!("no cache dir available"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Atomic replace via tmp file — same pattern as Swift's NSFileManager.replaceItemAt.
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string(cache)?;
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

#[allow(dead_code)]
pub fn wipe_all(override_dir: Option<&Path>) -> anyhow::Result<()> {
    if let Some(root) = cache_root(override_dir) {
        if root.exists() {
            fs::remove_dir_all(root)?;
        }
    }
    Ok(())
}

// ========================================================================
// Packed-slot arithmetic — mirrors Swift addPacked/applyFileDays/mergeFileDays.
// ========================================================================

/// Elementwise `a + sign*b`, clamped at 0. Handles mismatched slot counts
/// by treating missing slots as 0 in the shorter operand.
pub fn add_packed(a: &Packed, b: &Packed, sign: i64) -> Packed {
    let len = a.len().max(b.len());
    let mut out = vec![0i64; len];
    for (i, slot) in out.iter_mut().enumerate() {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        *slot = (av + sign * bv).max(0);
    }
    out
}

/// Apply a file's per-day packed contribution to the aggregate `cache.days`,
/// either adding (`sign=+1`) when registering a file's parse output or
/// subtracting (`sign=-1`) when evicting a stale entry.
pub fn apply_file_days(
    cache: &mut CostUsageCache,
    file_days: &HashMap<String, HashMap<String, Packed>>,
    sign: i64,
) {
    for (day, models) in file_days {
        let day_models = cache.days.entry(day.clone()).or_default();
        for (model, packed) in models {
            let existing = day_models.get(model).cloned().unwrap_or_default();
            let merged = add_packed(&existing, packed, sign);
            if merged.iter().all(|v| *v == 0) {
                day_models.remove(model);
            } else {
                day_models.insert(model.clone(), merged);
            }
        }
        if day_models.is_empty() {
            cache.days.remove(day);
        }
    }
}

/// Add a parse-delta's per-day contribution onto an existing file entry's
/// `days` map (for the incremental-parse path — merge new tail's output
/// into what was already cached for the same file).
pub fn merge_file_days(
    existing: &mut HashMap<String, HashMap<String, Packed>>,
    delta: &HashMap<String, HashMap<String, Packed>>,
) {
    for (day, models) in delta {
        let day_models = existing.entry(day.clone()).or_default();
        for (model, packed) in models {
            let merged = add_packed(day_models.get(model).unwrap_or(&Vec::new()), packed, 1);
            if merged.iter().all(|v| *v == 0) {
                day_models.remove(model);
            } else {
                day_models.insert(model.clone(), merged);
            }
        }
        if day_models.is_empty() {
            existing.remove(day);
        }
    }
}

/// Drop days outside the current scan range from the aggregate — prevents
/// the cache from growing without bound. File-level `days` are also pruned.
pub fn prune_days(cache: &mut CostUsageCache, since_key: &str, until_key: &str) {
    cache
        .days
        .retain(|k, _| k.as_str() >= since_key && k.as_str() <= until_key);
    for entry in cache.files.values_mut() {
        entry
            .days
            .retain(|k, _| k.as_str() >= since_key && k.as_str() <= until_key);
    }
}

// ========================================================================
// Decision: what to do with a JSONL file on a scan
// ========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    /// File's (mtime, size) match the cache — skip parsing, reuse days.
    Unchanged,
    /// File has grown (or was never cached) — parse incrementally from
    /// the last saved offset. If no prior offset, starts at 0.
    Incremental { start_offset: i64 },
    /// File shrank, mtime older than cached, or any other oddity —
    /// subtract the cached contribution and do a full re-parse.
    FullReparse,
}

pub fn decide_action(entry: Option<&FileEntry>, mtime_ms: i64, size: i64) -> FileAction {
    let entry = match entry {
        Some(e) => e,
        None => return FileAction::Incremental { start_offset: 0 },
    };
    if entry.mtime_unix_ms == mtime_ms && entry.size == size {
        return FileAction::Unchanged;
    }
    // File grew and mtime advanced (or same mtime but bigger, e.g. append without
    // stat update): try incremental from the last parsed offset.
    if size > entry.size {
        let off = entry.parsed_bytes.unwrap_or(entry.size);
        if off >= 0 && off <= size {
            return FileAction::Incremental { start_offset: off };
        }
    }
    FileAction::FullReparse
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_packed_basic() {
        let a = vec![10, 20, 30];
        let b = vec![1, 2, 3];
        assert_eq!(add_packed(&a, &b, 1), vec![11, 22, 33]);
        assert_eq!(add_packed(&a, &b, -1), vec![9, 18, 27]);
    }

    #[test]
    fn add_packed_clamps_at_zero() {
        let a = vec![5, 0];
        let b = vec![10, 0];
        assert_eq!(add_packed(&a, &b, -1), vec![0, 0]);
    }

    #[test]
    fn add_packed_handles_mismatched_lengths() {
        let a = vec![10, 20];
        let b = vec![1, 2, 3, 4];
        assert_eq!(add_packed(&a, &b, 1), vec![11, 22, 3, 4]);
    }

    #[test]
    fn decide_action_unchanged_when_mtime_and_size_match() {
        let entry = FileEntry {
            mtime_unix_ms: 1000,
            size: 500,
            days: HashMap::new(),
            parsed_bytes: Some(500),
            last_model: None,
            last_totals: None,
            session_id: None,
        };
        assert_eq!(
            decide_action(Some(&entry), 1000, 500),
            FileAction::Unchanged
        );
    }

    #[test]
    fn decide_action_incremental_when_file_grew() {
        let entry = FileEntry {
            mtime_unix_ms: 1000,
            size: 500,
            days: HashMap::new(),
            parsed_bytes: Some(500),
            last_model: None,
            last_totals: None,
            session_id: None,
        };
        let action = decide_action(Some(&entry), 2000, 800);
        assert_eq!(action, FileAction::Incremental { start_offset: 500 });
    }

    #[test]
    fn decide_action_full_reparse_when_file_shrank() {
        let entry = FileEntry {
            mtime_unix_ms: 1000,
            size: 500,
            days: HashMap::new(),
            parsed_bytes: Some(500),
            last_model: None,
            last_totals: None,
            session_id: None,
        };
        assert_eq!(
            decide_action(Some(&entry), 2000, 400),
            FileAction::FullReparse
        );
    }

    #[test]
    fn decide_action_new_file_starts_at_zero() {
        let action = decide_action(None, 1000, 500);
        assert_eq!(action, FileAction::Incremental { start_offset: 0 });
    }

    #[test]
    fn apply_file_days_plus_then_minus_is_zero() {
        let mut cache = CostUsageCache::default();
        let mut file_days: HashMap<String, HashMap<String, Packed>> = HashMap::new();
        let mut models = HashMap::new();
        models.insert("gpt-5".to_string(), vec![100, 0, 50]);
        file_days.insert("2026-04-24".to_string(), models);

        apply_file_days(&mut cache, &file_days, 1);
        assert_eq!(cache.days["2026-04-24"]["gpt-5"], vec![100, 0, 50]);

        apply_file_days(&mut cache, &file_days, -1);
        assert!(cache.days.is_empty()); // evicted when all slots hit 0
    }

    #[test]
    fn save_load_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("cli-pulse-cache-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let mut cache = CostUsageCache {
            last_scan_unix_ms: 1234567890,
            ..Default::default()
        };
        let mut models = HashMap::new();
        models.insert("gpt-5".to_string(), vec![1000, 0, 200]);
        cache.days.insert("2026-04-24".to_string(), models);

        save("codex", &cache, Some(&tmp)).unwrap();
        let loaded = load("codex", Some(&tmp));
        assert_eq!(loaded.version, CACHE_SCHEMA_VERSION);
        assert_eq!(loaded.last_scan_unix_ms, 1234567890);
        assert_eq!(loaded.days["2026-04-24"]["gpt-5"], vec![1000, 0, 200]);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn load_handles_missing_file_gracefully() {
        let tmp =
            std::env::temp_dir().join(format!("cli-pulse-cache-missing-{}", std::process::id()));
        let cache = load("codex", Some(&tmp));
        assert_eq!(cache.version, CACHE_SCHEMA_VERSION);
        assert!(cache.files.is_empty());
        assert!(cache.days.is_empty());
    }

    #[test]
    fn prune_days_drops_out_of_range() {
        let mut cache = CostUsageCache::default();
        let make = || {
            let mut m = HashMap::new();
            m.insert("gpt-5".to_string(), vec![100, 0, 50]);
            m
        };
        cache.days.insert("2026-01-01".into(), make());
        cache.days.insert("2026-04-24".into(), make());
        cache.days.insert("2026-12-31".into(), make());

        prune_days(&mut cache, "2026-04-01", "2026-04-30");
        assert_eq!(cache.days.len(), 1);
        assert!(cache.days.contains_key("2026-04-24"));
    }
}
