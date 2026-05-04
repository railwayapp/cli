use anyhow::Result;

use super::types::*;
use super::{exec_command_in_container, parse_f64, parse_i64};

/// Connect to localhost:3306 (already inside container via SSH).
/// Uses MYSQL_PWD env for password. Matches dashboard's mysql-ssh.ts.
fn build_mysql_command() -> String {
    let my = "MYSQL_PWD=\"$MYSQLPASSWORD\" mysql -h localhost -P 3306 -u \"$MYSQLUSER\" -D \"$MYSQLDATABASE\" --batch --skip-column-names";
    format!(
        r#"echo '===STATUS===';
{my} -e "SHOW GLOBAL STATUS" 2>/dev/null;
echo '===VARIABLES===';
{my} -e "SHOW VARIABLES WHERE Variable_name IN ('max_connections','innodb_buffer_pool_size','version','slow_query_log')" 2>/dev/null;
echo '===TABLES===';
{my} -e "SELECT TABLE_NAME, DATA_LENGTH, INDEX_LENGTH FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE() ORDER BY (DATA_LENGTH + INDEX_LENGTH) DESC LIMIT 20" 2>/dev/null
"#
    )
}

pub async fn fetch_mysql_stats(service_instance_id: &str) -> Result<MySqlStats> {
    let cmd = build_mysql_command();
    let output = exec_command_in_container(service_instance_id, &cmd).await?;
    parse_mysql_output(&output)
}

fn parse_mysql_output(output: &str) -> Result<MySqlStats> {
    let sections = super::split_sections(output);
    let mut stats = MySqlStats::default();

    // Parse SHOW GLOBAL STATUS into a map
    let status = if let Some(tsv) = sections.get("STATUS") {
        parse_tsv_map(tsv)
    } else {
        std::collections::HashMap::new()
    };

    // Parse SHOW VARIABLES into a map
    let vars = if let Some(tsv) = sections.get("VARIABLES") {
        parse_tsv_map(tsv)
    } else {
        std::collections::HashMap::new()
    };

    // Connections
    stats.connections = MySqlConnections {
        threads_connected: get_status_i64(&status, "Threads_connected"),
        threads_running: get_status_i64(&status, "Threads_running"),
        max_used_connections: get_status_i64(&status, "Max_used_connections"),
        max_connections: vars
            .get("max_connections")
            .map(|s| parse_i64(s))
            .unwrap_or(0),
        aborted_connects: get_status_i64(&status, "Aborted_connects"),
    };

    // Buffer pool
    let read_requests = get_status_f64(&status, "Innodb_buffer_pool_read_requests");
    let reads = get_status_f64(&status, "Innodb_buffer_pool_reads");
    let pool_size = vars
        .get("innodb_buffer_pool_size")
        .map(|s| parse_i64(s))
        .unwrap_or(0);
    let pool_data = get_status_i64(&status, "Innodb_buffer_pool_bytes_data");

    stats.buffer_pool = MySqlBufferPool {
        hit_ratio: buffer_pool_hit_ratio(read_requests, reads),
        usage_pct: if pool_size > 0 {
            pool_data as f64 / pool_size as f64 * 100.0
        } else {
            0.0
        },
        total_bytes: pool_size,
    };

    // Query stats
    stats.queries = MySqlQueryStats {
        selects: get_status_i64(&status, "Com_select"),
        inserts: get_status_i64(&status, "Com_insert"),
        updates: get_status_i64(&status, "Com_update"),
        deletes: get_status_i64(&status, "Com_delete"),
        slow_queries: get_status_i64(&status, "Slow_queries"),
        questions: get_status_i64(&status, "Questions"),
    };

    // InnoDB row operations
    stats.innodb = MySqlInnodb {
        row_reads: get_status_i64(&status, "Innodb_rows_read"),
        row_inserts: get_status_i64(&status, "Innodb_rows_inserted"),
        row_updates: get_status_i64(&status, "Innodb_rows_updated"),
        row_deletes: get_status_i64(&status, "Innodb_rows_deleted"),
    };

    // Table sizes
    if let Some(tsv) = sections.get("TABLES") {
        stats.table_sizes = parse_table_sizes(tsv);
    }

    Ok(stats)
}

fn buffer_pool_hit_ratio(read_requests: f64, reads: f64) -> f64 {
    if read_requests <= 0.0 {
        return 0.0;
    }

    ((read_requests - reads) / read_requests).clamp(0.0, 1.0)
}

fn parse_tsv_map(tsv: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for line in tsv.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, '\t').collect();
        if parts.len() == 2 {
            map.insert(parts[0].to_string(), parts[1].to_string());
        }
    }
    map
}

fn get_status_i64(map: &std::collections::HashMap<String, String>, key: &str) -> i64 {
    map.get(key).map(|s| parse_i64(s)).unwrap_or(0)
}

fn get_status_f64(map: &std::collections::HashMap<String, String>, key: &str) -> f64 {
    map.get(key).map(|s| parse_f64(s)).unwrap_or(0.0)
}

fn parse_table_sizes(tsv: &str) -> Vec<MySqlTableSize> {
    tsv.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 3 {
                let data = parse_i64(parts[1]);
                let index = parse_i64(parts[2]);
                Some(MySqlTableSize {
                    table_name: parts[0].to_string(),
                    data_bytes: data,
                    index_bytes: index,
                    total_bytes: data + index,
                })
            } else {
                None
            }
        })
        .collect()
}
