use serde::Serialize;

/// Wrapper enum for all database-specific metrics.
/// Tagged by database_type in JSON output.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "database_type")]
pub enum DatabaseStats {
    PostgreSQL(PostgresStats),
    Redis(RedisStats),
    MySQL(MySqlStats),
    MongoDB(MongoStats),
}

// ─── PostgreSQL ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Default)]
pub struct PostgresStats {
    pub connections: PgConnections,
    pub cache: PgCache,
    pub database_size: PgDatabaseSize,
    pub deadlocks: i64,
    pub table_stats: Vec<PgTableStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_stats: Option<Vec<PgQueryStats>>,
    pub vacuum_health: Vec<PgVacuumHealth>,
    pub index_health: PgIndexHealth,
    /// Tables with high sequential scans but no index scans (candidates for new indexes)
    pub missing_indexes: Vec<PgMissingIndex>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PgConnections {
    pub total: i64,
    pub active: i64,
    pub idle: i64,
    pub idle_in_transaction: i64,
    pub max_connections: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PgCache {
    pub hit_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PgDatabaseSize {
    pub total_bytes: i64,
    pub tables_bytes: i64,
    pub indexes_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PgTableStats {
    pub table_name: String,
    pub size_bytes: i64,
    pub seq_scan: i64,
    pub idx_scan: i64,
    pub live_tuples: i64,
    pub dead_tuples: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PgQueryStats {
    pub query: String,
    pub calls: i64,
    pub total_time_ms: f64,
    pub mean_time_ms: f64,
    pub rows: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PgVacuumHealth {
    pub table_name: String,
    pub dead_rows_pct: f64,
    pub last_vacuum: Option<String>,
    pub last_analyze: Option<String>,
    pub xid_age: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PgMissingIndex {
    pub table_name: String,
    pub live_rows: i64,
    pub seq_scan: i64,
    pub idx_scan: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PgIndexHealth {
    pub unused_indexes: Vec<String>,
    pub total_index_count: i64,
    pub unused_bytes: i64,
}

// ─── Redis ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Default)]
pub struct RedisStats {
    pub server: RedisServerInfo,
    pub memory: RedisMemoryInfo,
    pub throughput: RedisThroughput,
    pub cache: RedisCacheStats,
    pub persistence: RedisPersistence,
    pub keyspace: Vec<RedisKeyspaceDb>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RedisServerInfo {
    pub version: String,
    pub uptime_seconds: i64,
    pub connected_clients: i64,
    pub blocked_clients: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RedisMemoryInfo {
    pub used_bytes: i64,
    pub rss_bytes: i64,
    pub peak_bytes: i64,
    pub fragmentation_ratio: f64,
    pub max_memory_bytes: i64,
    pub eviction_policy: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RedisThroughput {
    pub ops_per_sec: f64,
    pub total_commands: i64,
    pub total_connections: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RedisCacheStats {
    pub hit_rate: f64,
    pub hits: i64,
    pub misses: i64,
    pub expired_keys: i64,
    pub evicted_keys: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RedisPersistence {
    pub rdb_last_save_status: String,
    pub aof_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RedisKeyspaceDb {
    pub db_index: i32,
    pub keys: i64,
    pub expires: i64,
    pub avg_ttl: i64,
}

// ─── MySQL ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Default)]
pub struct MySqlStats {
    pub connections: MySqlConnections,
    pub buffer_pool: MySqlBufferPool,
    pub queries: MySqlQueryStats,
    pub innodb: MySqlInnodb,
    pub table_sizes: Vec<MySqlTableSize>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MySqlConnections {
    pub threads_connected: i64,
    pub threads_running: i64,
    pub max_used_connections: i64,
    pub max_connections: i64,
    pub aborted_connects: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MySqlBufferPool {
    pub hit_ratio: f64,
    pub usage_pct: f64,
    pub total_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MySqlQueryStats {
    pub selects: i64,
    pub inserts: i64,
    pub updates: i64,
    pub deletes: i64,
    pub slow_queries: i64,
    pub questions: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MySqlInnodb {
    pub row_reads: i64,
    pub row_inserts: i64,
    pub row_updates: i64,
    pub row_deletes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MySqlTableSize {
    pub table_name: String,
    pub data_bytes: i64,
    pub index_bytes: i64,
    pub total_bytes: i64,
}

// ─── MongoDB ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Default)]
pub struct MongoStats {
    pub connections: MongoConnections,
    pub operations: MongoOperations,
    pub memory: MongoMemory,
    pub wired_tiger: MongoWiredTiger,
    pub collection_stats: Vec<MongoCollectionStats>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MongoConnections {
    pub current: i64,
    pub available: i64,
    pub total_created: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MongoOperations {
    pub insert: i64,
    pub query: i64,
    pub update: i64,
    pub delete: i64,
    pub command: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MongoMemory {
    pub resident_mb: i64,
    pub virtual_mb: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MongoWiredTiger {
    pub cache_used_bytes: i64,
    pub cache_max_bytes: i64,
    pub cache_dirty_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MongoCollectionStats {
    pub name: String,
    pub count: i64,
    pub size_bytes: i64,
    pub index_count: i64,
}
