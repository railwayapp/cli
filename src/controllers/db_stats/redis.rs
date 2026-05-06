use anyhow::Result;

use super::types::*;
use super::{exec_command_in_container, parse_f64, parse_i64};

/// Connect to localhost:6379 (already inside container via SSH).
/// Uses REDISPASSWORD from container env if set. Matches dashboard's redis-ssh.ts.
const REDIS_STATS_COMMAND: &str = "redis-cli -h localhost -p 6379 ${REDISPASSWORD:+-a \"$REDISPASSWORD\"} --no-auth-warning INFO all";

pub async fn fetch_redis_stats(service_instance_id: &str) -> Result<RedisStats> {
    let output = exec_command_in_container(service_instance_id, REDIS_STATS_COMMAND).await?;
    parse_redis_output(&output)
}

fn parse_redis_output(output: &str) -> Result<RedisStats> {
    let mut stats = RedisStats::default();
    let info = parse_info_map(output);

    // Server
    stats.server = RedisServerInfo {
        version: info.get("redis_version").cloned().unwrap_or_default(),
        uptime_seconds: info
            .get("uptime_in_seconds")
            .map(|s| parse_i64(s))
            .unwrap_or(0),
        connected_clients: info
            .get("connected_clients")
            .map(|s| parse_i64(s))
            .unwrap_or(0),
        blocked_clients: info
            .get("blocked_clients")
            .map(|s| parse_i64(s))
            .unwrap_or(0),
    };

    // Memory
    stats.memory = RedisMemoryInfo {
        used_bytes: info.get("used_memory").map(|s| parse_i64(s)).unwrap_or(0),
        rss_bytes: info
            .get("used_memory_rss")
            .map(|s| parse_i64(s))
            .unwrap_or(0),
        peak_bytes: info
            .get("used_memory_peak")
            .map(|s| parse_i64(s))
            .unwrap_or(0),
        fragmentation_ratio: info
            .get("mem_fragmentation_ratio")
            .map(|s| parse_f64(s))
            .unwrap_or(0.0),
        max_memory_bytes: info.get("maxmemory").map(|s| parse_i64(s)).unwrap_or(0),
        eviction_policy: info.get("maxmemory_policy").cloned().unwrap_or_default(),
    };

    // Throughput
    stats.throughput = RedisThroughput {
        ops_per_sec: info
            .get("instantaneous_ops_per_sec")
            .map(|s| parse_f64(s))
            .unwrap_or(0.0),
        total_commands: info
            .get("total_commands_processed")
            .map(|s| parse_i64(s))
            .unwrap_or(0),
        total_connections: info
            .get("total_connections_received")
            .map(|s| parse_i64(s))
            .unwrap_or(0),
    };

    // Cache
    let hits = info.get("keyspace_hits").map(|s| parse_i64(s)).unwrap_or(0);
    let misses = info
        .get("keyspace_misses")
        .map(|s| parse_i64(s))
        .unwrap_or(0);
    let total = hits + misses;
    stats.cache = RedisCacheStats {
        hit_rate: if total > 0 {
            hits as f64 / total as f64
        } else {
            0.0
        },
        hits,
        misses,
        expired_keys: info.get("expired_keys").map(|s| parse_i64(s)).unwrap_or(0),
        evicted_keys: info.get("evicted_keys").map(|s| parse_i64(s)).unwrap_or(0),
    };

    // Persistence
    stats.persistence = RedisPersistence {
        rdb_last_save_status: info
            .get("rdb_last_bgsave_status")
            .cloned()
            .unwrap_or_default(),
        aof_enabled: info.get("aof_enabled").map(|s| s == "1").unwrap_or(false),
    };

    // Keyspace (db0:keys=100,expires=50,avg_ttl=3600)
    stats.keyspace = parse_keyspace(&info);

    Ok(stats)
}

fn parse_info_map(output: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for line in output.lines() {
        let line = line.trim();
        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            map.insert(key.to_string(), value.to_string());
        }
    }
    map
}

fn parse_keyspace(info: &std::collections::HashMap<String, String>) -> Vec<RedisKeyspaceDb> {
    let mut dbs = Vec::new();
    for (key, value) in info {
        if let Some(idx_str) = key.strip_prefix("db") {
            if let Ok(idx) = idx_str.parse::<i32>() {
                let mut keys = 0i64;
                let mut expires = 0i64;
                let mut avg_ttl = 0i64;
                for part in value.split(',') {
                    if let Some((k, v)) = part.split_once('=') {
                        match k {
                            "keys" => keys = parse_i64(v),
                            "expires" => expires = parse_i64(v),
                            "avg_ttl" => avg_ttl = parse_i64(v),
                            _ => {}
                        }
                    }
                }
                dbs.push(RedisKeyspaceDb {
                    db_index: idx,
                    keys,
                    expires,
                    avg_ttl,
                });
            }
        }
    }
    dbs.sort_by_key(|d| d.db_index);
    dbs
}
