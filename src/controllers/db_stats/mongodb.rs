use anyhow::Result;
use serde_json::Value;

use super::exec_command_in_container;
use super::types::*;

/// Connect to localhost:27017 (already inside container via SSH).
/// Builds auth URL from MONGO* env vars. Matches dashboard's mongo-ssh.ts.
const MONGO_STATS_COMMAND: &str = r#"mongosh "mongodb://$MONGOUSER:$MONGOPASSWORD@localhost:27017/admin?authSource=admin" --quiet --norc --json=relaxed --eval "
function databaseNameFromUrl(url) {
    if (!url) {
        return '';
    }
    try {
        var parsed = new URL(url);
        return decodeURIComponent(parsed.pathname.replace(/^\/+/, '').split('/')[0] || '');
    } catch (e) {
        return '';
    }
}
var env = (typeof process !== 'undefined' && process.env) ? process.env : {};
var dbName = env.MONGODATABASE || env.MONGO_DATABASE || databaseNameFromUrl(env.MONGO_URL);
var targetDb = dbName ? db.getSiblingDB(dbName) : db;
var status = db.serverStatus();
var dbStats = targetDb.stats();
var colls = targetDb.getCollectionNames().slice(0, 20).map(function(c) {
    var s = targetDb.getCollection(c).stats();
    return {name: c, count: s.count || 0, size: s.size || 0, nindexes: s.nindexes || 0};
});
print(JSON.stringify({server: status, dbStats: dbStats, database: targetDb.getName(), collections: colls}));
" 2>/dev/null"#;

pub async fn fetch_mongo_stats(service_instance_id: &str) -> Result<MongoStats> {
    let output = exec_command_in_container(service_instance_id, MONGO_STATS_COMMAND).await?;
    parse_mongo_output(&output)
}

fn parse_mongo_output(output: &str) -> Result<MongoStats> {
    // The output may contain non-JSON lines (mongosh warnings, etc.)
    // Find the JSON line (starts with '{')
    let json_str = output
        .lines()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| anyhow::anyhow!("No JSON output from mongosh"))?;

    let root: Value = serde_json::from_str(json_str)?;
    let server = &root["server"];
    let _db_stats = &root["dbStats"];

    let mut stats = MongoStats::default();

    // Connections
    if let Some(conn) = server.get("connections") {
        stats.connections = MongoConnections {
            current: val_i64(conn, "current"),
            available: val_i64(conn, "available"),
            total_created: val_i64(conn, "totalCreated"),
        };
    }

    // Operations (opcounters)
    if let Some(ops) = server.get("opcounters") {
        stats.operations = MongoOperations {
            insert: val_i64(ops, "insert"),
            query: val_i64(ops, "query"),
            update: val_i64(ops, "update"),
            delete: val_i64(ops, "delete"),
            command: val_i64(ops, "command"),
        };
    }

    // Memory
    if let Some(mem) = server.get("mem") {
        stats.memory = MongoMemory {
            resident_mb: val_i64(mem, "resident"),
            virtual_mb: val_i64(mem, "virtual"),
        };
    }

    // WiredTiger cache
    if let Some(wt) = server.get("wiredTiger") {
        if let Some(cache) = wt.get("cache") {
            stats.wired_tiger = MongoWiredTiger {
                cache_used_bytes: val_i64(cache, "bytes currently in the cache"),
                cache_max_bytes: val_i64(cache, "maximum bytes configured"),
                cache_dirty_bytes: val_i64(cache, "tracked dirty bytes in the cache"),
            };
        }
    }

    // Collection stats
    if let Some(colls) = root.get("collections").and_then(|c| c.as_array()) {
        stats.collection_stats = colls
            .iter()
            .map(|c| MongoCollectionStats {
                name: c["name"].as_str().unwrap_or("").to_string(),
                count: val_i64(c, "count"),
                size_bytes: val_i64(c, "size"),
                index_count: val_i64(c, "nindexes"),
            })
            .collect();
    }

    Ok(stats)
}

fn val_i64(obj: &Value, key: &str) -> i64 {
    obj.get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
}
