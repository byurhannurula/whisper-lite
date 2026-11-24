//! Local transcript history.
//!
//! A capped JSON file rather than SQLite. A real database was the original plan, but the working set here
//! is a few hundred short strings that only ever need substring search — a database would be a
//! dependency and a schema for no gain at this size. Revisit if entries ever run to five figures.
//!
//! Text only. Audio is never written to disk, and the file is pruned on load by both age and
//! count so it cannot grow without bound.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Hard ceiling regardless of the retention window, so a heavy day cannot bloat the file.
const MAX_ENTRIES: usize = 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Unique per entry, so deletes do not depend on `at`, which two entries in one second share.
    /// Microsecond-scale, which keeps it below 2^53 so the webview reads it back exactly.
    /// Files from before this field load as 0 and get one assigned in `load`.
    #[serde(default)]
    pub id: u64,
    /// Unix seconds.
    pub at: u64,
    pub text: String,
    /// Seconds of audio that produced it.
    pub duration: f32,
}

fn path() -> PathBuf {
    crate::settings::dir().join("history.json")
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn load() -> Vec<Entry> {
    let p = path();
    if !p.exists() {
        return Vec::new();
    }
    let mut entries: Vec<Entry> = std::fs::read_to_string(&p)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    // Persist the migration at once: the settings window lists and later deletes by id, and both
    // calls load from disk, so the ids must not change between them.
    if assign_missing_ids(&mut entries) {
        let _ = save(&entries);
    }
    entries
}

fn assign_missing_ids(entries: &mut [Entry]) -> bool {
    let mut changed = false;
    for (i, e) in entries.iter_mut().enumerate() {
        if e.id == 0 {
            e.id = e.at * 1_000_000 + i as u64 % 1_000_000;
            changed = true;
        }
    }
    changed
}

fn next_id(entries: &[Entry]) -> u64 {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let max = entries.iter().map(|e| e.id).max().unwrap_or(0);
    micros.max(max + 1)
}

fn save(entries: &[Entry]) -> Result<()> {
    let p = path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(entries).context("serialising history")?;

    // Write-then-rename, so a crash mid-write cannot truncate the file.
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// Drops entries older than `retention_days`, then trims to `MAX_ENTRIES`.
///
/// `retention_days == 0` means keep forever, subject only to the count cap.
pub fn prune(mut entries: Vec<Entry>, retention_days: u32) -> Vec<Entry> {
    if retention_days > 0 {
        let cutoff = now().saturating_sub(retention_days as u64 * 86_400);
        entries.retain(|e| e.at >= cutoff);
    }
    // Newest first, so truncation drops the oldest.
    entries.sort_by_key(|e| std::cmp::Reverse(e.at));
    entries.truncate(MAX_ENTRIES);
    entries
}

pub fn append(text: &str, duration: f32, retention_days: u32) -> Result<()> {
    let mut entries = load();
    let id = next_id(&entries);
    entries.insert(
        0,
        Entry {
            id,
            at: now(),
            text: text.to_string(),
            duration,
        },
    );
    save(&prune(entries, retention_days))
}

pub fn delete(id: u64) -> Result<()> {
    let entries: Vec<Entry> = load().into_iter().filter(|e| e.id != id).collect();
    save(&entries)
}

pub fn clear() -> Result<()> {
    let p = path();
    if p.exists() {
        std::fs::remove_file(&p)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(at: u64, text: &str) -> Entry {
        Entry {
            id: 0,
            at,
            text: text.into(),
            duration: 1.0,
        }
    }

    #[test]
    fn prune_drops_entries_past_the_window() {
        let old = now() - 40 * 86_400;
        let recent = now() - 86_400;
        let kept = prune(vec![entry(old, "old"), entry(recent, "recent")], 30);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].text, "recent");
    }

    #[test]
    fn zero_retention_keeps_everything() {
        let ancient = 1_000;
        let kept = prune(vec![entry(ancient, "ancient")], 0);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn prune_sorts_newest_first() {
        let kept = prune(vec![entry(100, "older"), entry(200, "newer")], 0);
        assert_eq!(kept[0].text, "newer");
    }

    #[test]
    fn same_second_entries_get_distinct_ids() {
        let mut entries = vec![entry(500, "a"), entry(500, "b")];
        assert!(assign_missing_ids(&mut entries));
        assert_ne!(entries[0].id, entries[1].id);
        assert!(!assign_missing_ids(&mut entries));
    }

    #[test]
    fn next_id_is_above_every_existing_id() {
        let mut far_future = entry(0, "x");
        far_future.id = u64::MAX / 2;
        assert!(next_id(&[far_future]) > u64::MAX / 2);
    }

    #[test]
    fn ids_fit_in_a_js_number() {
        assert!(next_id(&[]) < 1 << 53);
    }

    #[test]
    fn prune_enforces_the_count_cap() {
        let many: Vec<Entry> = (0..MAX_ENTRIES as u64 + 50)
            .map(|i| entry(i, "x"))
            .collect();
        assert_eq!(prune(many, 0).len(), MAX_ENTRIES);
    }
}
