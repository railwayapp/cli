//! Small in-memory cache for remote directory listings.
//!
//! The cache backs two UX features in the volume/service file browser:
//!
//! 1. Stale-while-revalidate navigation: when the user opens a directory we've
//!    already seen, render the cached entries instantly and only re-fetch in
//!    the background.
//! 2. Optimistic mutations: deletes/uploads/edits patch the cached entries
//!    immediately, then a background fetch reconciles with the server.
//!
//! The cache is bounded (`MAX_ENTRIES`) and entries have a TTL (`TTL`) after
//! which they are considered stale. Stale entries are still served, but trigger
//! a revalidation. The cache is owned by `VolumeBrowserApp`; concurrency is
//! avoided by doing all mutations on the main task and only spawning fetches.
//!
//! Keys are normalized remote directory paths (e.g. `/`, `/data/backups`).

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::commands::volume::sftp::VolumeFileEntry;

use super::app::{join_remote_path, normalize_remote_dir};

/// How long a cached directory listing is considered fresh. After this, the
/// entry is still returned (so the UI updates instantly) but a background
/// revalidation is triggered.
pub const TTL: Duration = Duration::from_secs(30);

/// Maximum number of directories to cache before evicting the least recently
/// used one.
pub const MAX_ENTRIES: usize = 64;

#[derive(Debug, Clone)]
pub struct CachedDir {
    pub entries: Vec<VolumeFileEntry>,
    pub fetched_at: Instant,
    pub last_used: Instant,
}

impl CachedDir {
    fn new(entries: Vec<VolumeFileEntry>, now: Instant) -> Self {
        Self {
            entries,
            fetched_at: now,
            last_used: now,
        }
    }

    pub fn is_fresh(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.fetched_at) < TTL
    }
}

/// Result of looking up a directory in the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Miss,
    Fresh,
    Stale,
}

#[derive(Debug, Default)]
pub struct DirCache {
    map: HashMap<String, CachedDir>,
}

impl DirCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached entries for `dir` and the freshness of the lookup.
    /// Touches the entry's `last_used` so LRU eviction is accurate. Returns
    /// `(None, Lookup::Miss)` if there's no cached entry.
    pub fn get(&mut self, dir: &str) -> (Option<&[VolumeFileEntry]>, Lookup) {
        let key = normalize_remote_dir(dir);
        let now = Instant::now();
        match self.map.get_mut(&key) {
            Some(cached) => {
                let lookup = if cached.is_fresh(now) {
                    Lookup::Fresh
                } else {
                    Lookup::Stale
                };
                cached.last_used = now;
                (Some(cached.entries.as_slice()), lookup)
            }
            None => (None, Lookup::Miss),
        }
    }

    /// Inserts or replaces the cached entries for `dir`.
    pub fn insert(&mut self, dir: &str, entries: Vec<VolumeFileEntry>) {
        let key = normalize_remote_dir(dir);
        let now = Instant::now();
        self.map.insert(key, CachedDir::new(entries, now));
        self.enforce_capacity();
    }

    /// Drops the cached entry for `dir` and every cached descendant. Used after
    /// deleting a directory: every cached path beneath it is now bogus.
    pub fn invalidate_subtree(&mut self, dir: &str) {
        let key = normalize_remote_dir(dir);
        self.map
            .retain(|cached_key, _| !(cached_key == &key || is_descendant(cached_key, &key)));
    }

    /// Optimistically removes a child entry from the cached listing of `parent`.
    /// No-op if `parent` is not cached or `name` isn't present.
    pub fn apply_delete(&mut self, parent: &str, name: &str) {
        let key = normalize_remote_dir(parent);
        if let Some(cached) = self.map.get_mut(&key) {
            cached.entries.retain(|entry| entry.name != name);
            cached.last_used = Instant::now();
        }
    }

    /// Optimistically inserts/replaces an entry in the cached listing of
    /// `parent`. No-op if `parent` is not cached.
    pub fn apply_upsert(&mut self, parent: &str, new_entry: VolumeFileEntry) {
        let key = normalize_remote_dir(parent);
        if let Some(cached) = self.map.get_mut(&key) {
            if let Some(existing) = cached
                .entries
                .iter_mut()
                .find(|entry| entry.name == new_entry.name)
            {
                *existing = new_entry;
            } else {
                cached.entries.push(new_entry);
                cached
                    .entries
                    .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            }
            cached.last_used = Instant::now();
        }
    }

    /// Returns the immediate child directories that are not currently cached.
    /// Used by the prefetch logic.
    pub fn missing_children(
        &self,
        parent: &str,
        entries: &[VolumeFileEntry],
        cap: usize,
    ) -> Vec<String> {
        if cap == 0 {
            return Vec::new();
        }
        let parent = normalize_remote_dir(parent);
        entries
            .iter()
            .filter(|entry| entry.kind == "directory")
            .map(|entry| join_remote_path(&parent, &entry.name))
            .map(|path| normalize_remote_dir(&path))
            .filter(|path| !self.map.contains_key(path))
            .take(cap)
            .collect()
    }

    fn enforce_capacity(&mut self) {
        while self.map.len() > MAX_ENTRIES {
            // Evict the least recently used entry. Linear scan is fine at this
            // size (MAX_ENTRIES is tiny).
            let lru_key = self
                .map
                .iter()
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(key, _)| key.clone());
            match lru_key {
                Some(key) => {
                    self.map.remove(&key);
                }
                None => break,
            }
        }
    }
}

fn is_descendant(candidate: &str, ancestor: &str) -> bool {
    if ancestor == "/" {
        // Everything is a descendant of root, but invalidating root via
        // invalidate_subtree should drop the whole cache.
        return candidate != "/";
    }
    candidate
        .strip_prefix(ancestor)
        .is_some_and(|suffix| suffix.starts_with('/'))
}
