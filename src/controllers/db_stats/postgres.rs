use anyhow::Result;
use csv::ReaderBuilder;

use super::types::*;
use super::{exec_command_in_container, parse_f64, parse_i64, split_sections};

/// Build the psql stats command. Connects to localhost:5432 with SSL disabled
/// (we're already inside the container via SSH). Uses PGUSER/PGPASSWORD/PGDATABASE
/// from the container env. This matches the dashboard's approach in postgres-ssh.ts.
fn build_postgres_command() -> String {
    let pg =
        "PGHOST=localhost PGPORT=5432 PGSSLMODE=disable psql --csv -q -P pager=off -P footer=off";
    let pgt = "PGHOST=localhost PGPORT=5432 PGSSLMODE=disable psql -t -A -q";
    format!(
        r#"echo '===CONNECTIONS===';
{pg} -c "SELECT COALESCE(state, 'total') as state, count(*) as count FROM pg_stat_activity WHERE datname = current_database() GROUP BY ROLLUP(state)";
echo '===MAX_CONN===';
{pgt} -c "SHOW max_connections";
echo '===CACHE_AND_DEADLOCKS===';
{pg} -c "SELECT COALESCE(sum(heap_blks_hit)::float / NULLIF(sum(heap_blks_hit) + sum(heap_blks_read), 0), 0) as cache_hit_ratio, (SELECT deadlocks FROM pg_stat_database WHERE datname = current_database()) as deadlocks FROM pg_statio_user_tables";
echo '===SIZE===';
{pg} -c "SELECT pg_database_size(current_database()) as total, COALESCE((SELECT sum(pg_table_size(c.oid)) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind = 'r' AND n.nspname = 'public'), 0) as tables, COALESCE((SELECT sum(pg_indexes_size(c.oid)) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind = 'r' AND n.nspname = 'public'), 0) as indexes";
echo '===TABLES===';
{pg} -c "SELECT relname, pg_total_relation_size(relid) as size, seq_scan, idx_scan, n_live_tup, n_dead_tup FROM pg_stat_user_tables ORDER BY pg_total_relation_size(relid) DESC LIMIT 20";
echo '===QUERIES===';
{pg} -c "SELECT left(query, 120) as query, calls, total_exec_time, mean_exec_time, rows FROM pg_stat_statements WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) ORDER BY total_exec_time DESC LIMIT 10" 2>/dev/null || echo 'NOT_AVAILABLE';
echo '===VACUUM===';
{pg} -c "SELECT relname, n_dead_tup, n_live_tup, last_vacuum::text, last_analyze::text, age(relfrozenxid) as xid_age FROM pg_stat_user_tables s JOIN pg_class c ON c.oid = s.relid ORDER BY n_dead_tup DESC LIMIT 15";
echo '===INDEXES===';
{pg} -c "SELECT indexrelname, pg_relation_size(indexrelid) as index_size, idx_scan, (SELECT count(*) FROM pg_stat_user_indexes) as total_count FROM pg_stat_user_indexes WHERE idx_scan = 0 AND schemaname = 'public' LIMIT 20";
echo '===MISSING_INDEXES===';
{pg} -c "SELECT relname, n_live_tup, seq_scan, idx_scan FROM pg_stat_user_tables WHERE seq_scan > 0 AND idx_scan = 0 AND n_live_tup > 1000 ORDER BY seq_scan DESC LIMIT 10"
"#
    )
}

pub async fn fetch_postgres_stats(service_instance_id: &str) -> Result<PostgresStats> {
    let cmd = build_postgres_command();
    let output = exec_command_in_container(service_instance_id, &cmd).await?;
    parse_postgres_output(&output)
}

fn parse_postgres_output(output: &str) -> Result<PostgresStats> {
    let sections = split_sections(output);
    let mut stats = PostgresStats::default();

    // Connections
    if let Some(csv) = sections.get("CONNECTIONS") {
        stats.connections = parse_connections(csv);
    }

    // Max connections
    if let Some(val) = sections.get("MAX_CONN") {
        stats.connections.max_connections = parse_i64(val);
    }

    // Cache hit ratio + deadlocks (combined query)
    if let Some(csv) = sections.get("CACHE_AND_DEADLOCKS") {
        let rows = parse_csv_rows(csv);
        if let Some(row) = rows.first() {
            stats.cache.hit_ratio = row.first().map(|s| parse_f64(s)).unwrap_or(0.0);
            stats.deadlocks = row.get(1).map(|s| parse_i64(s)).unwrap_or(0);
        }
    }

    // Database size
    if let Some(csv) = sections.get("SIZE") {
        stats.database_size = parse_db_size(csv);
    }

    // Table stats
    if let Some(csv) = sections.get("TABLES") {
        stats.table_stats = parse_table_stats(csv);
    }

    // Query stats (optional -- pg_stat_statements may not be loaded)
    if let Some(csv) = sections.get("QUERIES") {
        if *csv != "NOT_AVAILABLE" && !csv.is_empty() {
            stats.query_stats = Some(parse_query_stats(csv));
        }
    }

    // Vacuum health
    if let Some(csv) = sections.get("VACUUM") {
        stats.vacuum_health = parse_vacuum_health(csv);
    }

    // Index health
    if let Some(csv) = sections.get("INDEXES") {
        stats.index_health = parse_index_health(csv);
    }

    // Missing indexes (tables that may benefit from an index)
    if let Some(csv) = sections.get("MISSING_INDEXES") {
        stats.missing_indexes = parse_missing_indexes(csv);
    }

    Ok(stats)
}

fn parse_csv_rows(csv: &str) -> Vec<Vec<String>> {
    ReaderBuilder::new()
        .has_headers(true)
        .from_reader(csv.as_bytes())
        .records()
        .filter_map(|record| record.ok())
        .map(|record| record.iter().map(|field| field.to_string()).collect())
        .collect()
}

fn parse_connections(csv: &str) -> PgConnections {
    let mut conn = PgConnections::default();
    for row in parse_csv_rows(csv) {
        if row.len() < 2 {
            continue;
        }
        let count = parse_i64(&row[1]);
        match row[0].as_str() {
            "active" => conn.active = count,
            "idle" => conn.idle = count,
            "idle in transaction" => conn.idle_in_transaction = count,
            "total" => conn.total = count,
            _ => {}
        }
    }
    // If total wasn't from ROLLUP, compute it
    if conn.total == 0 {
        conn.total = conn.active + conn.idle + conn.idle_in_transaction;
    }
    conn
}

fn parse_db_size(csv: &str) -> PgDatabaseSize {
    let rows = parse_csv_rows(csv);
    if let Some(row) = rows.first() {
        PgDatabaseSize {
            total_bytes: row.first().map(|s| parse_i64(s)).unwrap_or(0),
            tables_bytes: row.get(1).map(|s| parse_i64(s)).unwrap_or(0),
            indexes_bytes: row.get(2).map(|s| parse_i64(s)).unwrap_or(0),
        }
    } else {
        PgDatabaseSize::default()
    }
}

fn parse_table_stats(csv: &str) -> Vec<PgTableStats> {
    parse_csv_rows(csv)
        .into_iter()
        .filter(|row| row.len() >= 6)
        .map(|row| PgTableStats {
            table_name: row[0].clone(),
            size_bytes: parse_i64(&row[1]),
            seq_scan: parse_i64(&row[2]),
            idx_scan: parse_i64(&row[3]),
            live_tuples: parse_i64(&row[4]),
            dead_tuples: parse_i64(&row[5]),
        })
        .collect()
}

fn parse_query_stats(csv: &str) -> Vec<PgQueryStats> {
    parse_csv_rows(csv)
        .into_iter()
        .filter(|row| row.len() >= 5)
        .map(|row| PgQueryStats {
            query: row[0].clone(),
            calls: parse_i64(&row[1]),
            total_time_ms: parse_f64(&row[2]),
            mean_time_ms: parse_f64(&row[3]),
            rows: parse_i64(&row[4]),
        })
        .collect()
}

fn parse_vacuum_health(csv: &str) -> Vec<PgVacuumHealth> {
    parse_csv_rows(csv)
        .into_iter()
        .filter(|row| row.len() >= 5)
        .map(|row| {
            let dead = parse_f64(&row[1]);
            let live = parse_f64(&row[2]);
            let total = dead + live;
            PgVacuumHealth {
                table_name: row[0].clone(),
                dead_rows_pct: if total > 0.0 {
                    dead / total * 100.0
                } else {
                    0.0
                },
                last_vacuum: if row[3].is_empty() {
                    None
                } else {
                    Some(row[3].clone())
                },
                last_analyze: if row[4].is_empty() {
                    None
                } else {
                    Some(row[4].clone())
                },
                xid_age: row.get(5).map(|s| parse_i64(s)).unwrap_or(0),
            }
        })
        .collect()
}

fn parse_index_health(csv: &str) -> PgIndexHealth {
    let rows = parse_csv_rows(csv);
    let total = rows
        .first()
        .and_then(|r| r.get(3))
        .map(|s| parse_i64(s))
        .unwrap_or(0);
    let mut unused_indexes = Vec::new();
    let mut unused_bytes = 0i64;
    for row in &rows {
        if let Some(name) = row.first() {
            unused_indexes.push(name.clone());
            unused_bytes += row.get(1).map(|s| parse_i64(s)).unwrap_or(0);
        }
    }
    PgIndexHealth {
        unused_indexes,
        total_index_count: total,
        unused_bytes,
    }
}

fn parse_missing_indexes(csv: &str) -> Vec<PgMissingIndex> {
    parse_csv_rows(csv)
        .into_iter()
        .filter(|row| row.len() >= 4)
        .map(|row| PgMissingIndex {
            table_name: row[0].clone(),
            live_rows: parse_i64(&row[1]),
            seq_scan: parse_i64(&row[2]),
            idx_scan: parse_i64(&row[3]),
        })
        .collect()
}
