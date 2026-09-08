//! herdr parks a machine in Attention when a connection attempt fails in a way
//! it will not retry, and only the client log says so:
//! `WARN herdr::client: endpoint needs attention endpoint=ssh:<profile> …`.
//! The log sits next to the socket herdr hands us, so a sync can tell which
//! machines are stuck and toggle them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

const TAIL_BYTES: u64 = 512 * 1024;

pub fn client_log_path() -> Option<PathBuf> {
    let socket = std::env::var_os("HERDR_SOCKET_PATH")?;
    Some(Path::new(&socket).parent()?.join("herdr-client.log"))
}

/// Profiles whose last Attention entry is newer than our last toggle of them.
pub fn stuck_profiles(
    log: &str,
    kicked_at: &BTreeMap<String, String>,
    now: DateTime<Utc>,
) -> BTreeSet<String> {
    let mut latest: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
    for line in log.lines() {
        let Some(rest) = line.split("needs attention endpoint=ssh:").nth(1) else {
            continue;
        };
        let id: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        let Some(stamp) = line
            .split_whitespace()
            .next()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        else {
            continue;
        };
        let stamp = stamp.with_timezone(&Utc);
        if stamp <= now {
            latest.insert(id, stamp);
        }
    }
    latest
        .into_iter()
        .filter(|(id, at)| {
            kicked_at
                .get(id)
                .and_then(|k| DateTime::parse_from_rfc3339(k).ok())
                .is_none_or(|k| *at > k.with_timezone(&Utc))
        })
        .map(|(id, _)| id)
        .collect()
}

pub fn read_tail(path: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len > TAIL_BYTES {
        let _ = file.seek(SeekFrom::Start(len - TAIL_BYTES));
    }
    let mut buf = String::new();
    let _ = file.read_to_string(&mut buf);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
2026-09-08T14:35:28Z  WARN herdr::client: endpoint transport failed endpoint=ssh:aaaa error=endpoint health check timed out
2026-09-08T14:35:30Z  WARN herdr::client: endpoint needs attention endpoint=ssh:aaaa generation=7 error=unsupported remote platform: {\"status\":\"ready\"}
2026-09-08T14:40:00Z  WARN herdr::client: endpoint needs attention endpoint=ssh:bbbb generation=2 error=Permission denied
";

    #[test]
    fn attention_newer_than_our_last_toggle_counts_as_stuck() {
        let now = DateTime::parse_from_rfc3339("2026-09-08T15:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut kicked = BTreeMap::new();
        assert_eq!(
            stuck_profiles(LOG, &kicked, now),
            ["aaaa".to_string(), "bbbb".to_string()]
                .into_iter()
                .collect()
        );
        kicked.insert("aaaa".into(), "2026-09-08T14:36:00Z".into());
        assert_eq!(
            stuck_profiles(LOG, &kicked, now),
            ["bbbb".to_string()].into_iter().collect()
        );
        kicked.insert("bbbb".into(), "2026-09-08T14:39:00Z".into());
        assert_eq!(
            stuck_profiles(LOG, &kicked, now),
            ["bbbb".to_string()].into_iter().collect()
        );
    }
}
