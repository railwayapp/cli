use crate::{queries, subscriptions};
use colored::Colorize;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

const NETWORK_FLOW_TIME_WIDTH: usize = 30;
const NETWORK_FLOW_DIR_WIDTH: usize = 3;
const NETWORK_FLOW_PROTO_WIDTH: usize = 7;
const NETWORK_FLOW_ENDPOINT_WIDTH: usize = 47;
const NETWORK_FLOW_PEER_WIDTH: usize = 12;
const NETWORK_FLOW_TRAFFIC_WIDTH: usize = 10;
const NETWORK_FLOW_LATENCY_WIDTH: usize = 8;
const NETWORK_FLOW_STATUS_WIDTH: usize = 16;

// Trait for common fields on log types
pub trait LogLike {
    fn message(&self) -> &str;
    fn timestamp(&self) -> &str;
    fn attributes(&self) -> Vec<(&str, &str)>;
}

/// Format log line with attributes into a colored string for display to a string
pub fn format_attr_log_string<T: LogLike>(log: &T, show_all_attributes: bool) -> String {
    let timestamp = log.timestamp();
    let message = log.message();
    let attributes = log.attributes();

    // For some reason, we choose to only format the log if there are attributes
    // other than level (which is always present). This is likely because we
    // don't want to complicate the log for users who are just console logging
    // in their app without taking advantage of our structured logging.
    if attributes.is_empty() || (attributes.len() == 1 && attributes[0].0 == "level") {
        return message.to_string();
    }

    let mut level: Option<String> = None;
    let mut others = Vec::new();
    // format attributes other than level
    for (key, value) in attributes {
        match key.to_lowercase().as_str() {
            "level" | "lvl" | "severity" => level = Some(value.to_string()),
            _ => {
                if show_all_attributes {
                    others.push(format!(
                        "{}{}{}",
                        key.magenta(),
                        "=",
                        value
                            .normal()
                            .replace('"', "\"".dimmed().to_string().as_str())
                    ));
                }
            }
        }
    }

    // If we have a level, format with level indicator
    if let Some(level) = level {
        let level_str = match level.replace('"', "").to_lowercase().as_str() {
            "info" => "[INFO]".blue(),
            "error" | "err" => "[ERRO]".red(),
            "warn" => "[WARN]".yellow(),
            "debug" => "[DBUG]".dimmed(),
            _ => format!("[{level}]").normal(),
        }
        .bold();

        if others.is_empty() {
            format!("{} {}", level_str, message)
        } else {
            format!(
                "{} {} {} {}",
                timestamp.replace('"', "").normal(),
                level_str,
                message,
                others.join(" ")
            )
        }
    } else {
        // No level attribute, just return the message
        message.to_string()
    }
}

/// Formatting mode for log output
#[derive(Clone, Copy)]
pub enum LogFormat {
    /// Level indicator only (e.g. [ERRO]), no other attributes - good for build logs
    LevelOnly,
    /// Full formatting with all attributes - good for deploy logs
    Full,
}

/// Format a log entry as a string based
pub fn format_log_string<T>(log: T, json: bool, format: LogFormat) -> String
where
    T: LogLike + serde::Serialize,
{
    if json {
        // For JSON output, handle attributes specially
        let mut map: HashMap<String, Value> = HashMap::new();

        map.insert(
            "message".to_string(),
            serde_json::to_value(log.message()).unwrap(),
        );
        map.insert(
            "timestamp".to_string(),
            serde_json::to_value(log.timestamp()).unwrap(),
        );

        // Insert dynamic attributes
        for (key, value) in log.attributes() {
            let parsed_value = match value.trim_matches('"').parse::<Value>() {
                Ok(v) => v,
                Err(_) => serde_json::to_value(value.trim_matches('"')).unwrap(),
            };
            map.insert(key.to_string(), parsed_value);
        }

        serde_json::to_string(&map).unwrap()
    } else {
        match format {
            LogFormat::LevelOnly => format_attr_log_string(&log, false),
            LogFormat::Full => format_attr_log_string(&log, true),
        }
    }
}

/// Format a log entry as a string based and print it
pub fn print_log<T>(log: T, json: bool, format: LogFormat)
where
    T: LogLike + serde::Serialize,
{
    println!("{}", format_log_string(log, json, format));
}

pub trait HttpLogLike: serde::Serialize {
    fn timestamp(&self) -> &str;
    fn method(&self) -> &str;
    fn path(&self) -> &str;
    fn http_status(&self) -> i64;
    fn total_duration(&self) -> i64;
    fn request_id(&self) -> &str;
}

pub fn format_http_log_string<T: HttpLogLike>(log: &T, json: bool) -> String {
    if json {
        serde_json::to_string(log).unwrap()
    } else {
        let status = log.http_status();
        let status = match status {
            200..=299 => status.to_string().green(),
            300..=399 => status.to_string().cyan(),
            400..=499 => status.to_string().yellow(),
            500..=599 => status.to_string().red(),
            _ => status.to_string().normal(),
        };

        format!(
            "{} {} {} {} {} {}",
            log.timestamp().dimmed(),
            log.method().bold(),
            log.path(),
            status.bold(),
            format!("{}ms", log.total_duration()).dimmed(),
            log.request_id().dimmed()
        )
    }
}

pub fn print_http_log<T: HttpLogLike>(log: T, json: bool) {
    println!("{}", format_http_log_string(&log, json));
}

pub trait NetworkFlowLogLike: Serialize {
    fn capture_end(&self) -> &str;
    fn direction_value(&self) -> String;
    fn l4_protocol_value(&self) -> String;
    fn src_addr(&self) -> &str;
    fn src_port(&self) -> i64;
    fn dst_addr(&self) -> &str;
    fn dst_port(&self) -> i64;
    fn peer_kind_value(&self) -> String;
    fn byte_count(&self) -> i64;
    fn l4_latency_ms(&self) -> f64;
    fn drop_cause(&self) -> Option<&str>;
}

pub fn format_network_flow_log_header() -> String {
    format!(
        "{:<time_width$} {:<dir_width$} {:<proto_width$} {:<endpoint_width$} {:<endpoint_width$} {:<peer_width$} {:>traffic_width$} {:>latency_width$} {:<status_width$}",
        "Time".bold(),
        "Dir".bold(),
        "Proto".bold(),
        "Source".bold(),
        "Destination".bold(),
        "Peer".bold(),
        "Traffic".bold(),
        "Latency".bold(),
        "Status".bold(),
        time_width = NETWORK_FLOW_TIME_WIDTH,
        dir_width = NETWORK_FLOW_DIR_WIDTH,
        proto_width = NETWORK_FLOW_PROTO_WIDTH,
        endpoint_width = NETWORK_FLOW_ENDPOINT_WIDTH,
        peer_width = NETWORK_FLOW_PEER_WIDTH,
        traffic_width = NETWORK_FLOW_TRAFFIC_WIDTH,
        latency_width = NETWORK_FLOW_LATENCY_WIDTH,
        status_width = NETWORK_FLOW_STATUS_WIDTH,
    )
}

pub fn format_network_flow_log_string<T: NetworkFlowLogLike>(log: &T, json: bool) -> String {
    if json {
        let mut value = serde_json::to_value(log).unwrap();
        if let Value::Object(map) = &mut value {
            map.insert(
                "timestamp".to_string(),
                Value::String(log.capture_end().to_string()),
            );
        }
        return serde_json::to_string(&value).unwrap();
    }

    let direction = log.direction_value();
    let direction_label = direction_label(&direction);
    let protocol = log.l4_protocol_value().to_uppercase();
    let source = endpoint(log.src_addr(), log.src_port());
    let destination = endpoint(log.dst_addr(), log.dst_port());
    let peer = peer_label(&log.peer_kind_value());
    let traffic = format_bytes(log.byte_count());
    let latency = format_latency(log.l4_latency_ms());
    let status = log.drop_cause().unwrap_or("OK");

    format!(
        "{:<time_width$} {:<dir_width$} {:<proto_width$} {:<endpoint_width$} {:<endpoint_width$} {:<peer_width$} {:>traffic_width$} {:>latency_width$} {:<status_width$}",
        log.capture_end().dimmed(),
        direction_label,
        protocol.green(),
        source,
        destination,
        peer,
        traffic.dimmed(),
        latency.dimmed(),
        status_label(status),
        time_width = NETWORK_FLOW_TIME_WIDTH,
        dir_width = NETWORK_FLOW_DIR_WIDTH,
        proto_width = NETWORK_FLOW_PROTO_WIDTH,
        endpoint_width = NETWORK_FLOW_ENDPOINT_WIDTH,
        peer_width = NETWORK_FLOW_PEER_WIDTH,
        traffic_width = NETWORK_FLOW_TRAFFIC_WIDTH,
        latency_width = NETWORK_FLOW_LATENCY_WIDTH,
        status_width = NETWORK_FLOW_STATUS_WIDTH,
    )
}

pub fn print_network_flow_log<T: NetworkFlowLogLike>(log: T, json: bool) {
    println!("{}", format_network_flow_log_string(&log, json));
}

fn endpoint(addr: &str, port: i64) -> String {
    if addr.contains(':') {
        format!("[{addr}]:{port}")
    } else {
        format!("{addr}:{port}")
    }
}

fn direction_label(direction: &str) -> String {
    let arrow = direction_label_for_terminal(direction, is_dumb_terminal());

    match direction {
        "egress" => arrow.blue().bold().to_string(),
        "ingress" => arrow.normal().bold().to_string(),
        _ => arrow.yellow().bold().to_string(),
    }
}

fn direction_label_for_terminal(direction: &str, is_dumb: bool) -> &'static str {
    match direction {
        "ingress" => {
            if is_dumb {
                "in"
            } else {
                "↓"
            }
        }
        "egress" => {
            if is_dumb {
                "out"
            } else {
                "↑"
            }
        }
        _ => "?",
    }
}

fn is_dumb_terminal() -> bool {
    std::env::var("TERM").is_ok_and(|term| term == "dumb")
}

fn peer_label(peer_kind: &str) -> String {
    match peer_kind {
        "service" => "Service",
        "internet" => "Internet",
        "edge_proxy" => "Edge Proxy",
        "local_dns" => "DNS",
        "unknown" => "Unknown",
        other => other,
    }
    .to_string()
}

fn format_bytes(bytes: i64) -> String {
    let bytes = bytes as f64;
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes;
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{} {}", value as i64, UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_latency(latency_ms: f64) -> String {
    if latency_ms.fract() == 0.0 {
        format!("{}ms", latency_ms as i64)
    } else {
        format!("{latency_ms:.1}ms")
    }
}

fn status_label(status: &str) -> colored::ColoredString {
    if status == "OK" {
        status.green()
    } else {
        status.red()
    }
}

fn serialized_enum_value<T>(value: &T) -> String
where
    T: Serialize + std::fmt::Debug,
{
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{value:?}").to_ascii_lowercase())
}

// Implementations for all the generated GraphQL log types
impl LogLike for subscriptions::deployment_logs::LogFields {
    fn message(&self) -> &str {
        &self.message
    }
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn attributes(&self) -> Vec<(&str, &str)> {
        self.attributes
            .iter()
            .map(|a| (a.key.as_str(), a.value.as_str()))
            .collect()
    }
}

impl LogLike for queries::deployment_logs::LogFields {
    fn message(&self) -> &str {
        &self.message
    }
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn attributes(&self) -> Vec<(&str, &str)> {
        self.attributes
            .iter()
            .map(|a| (a.key.as_str(), a.value.as_str()))
            .collect()
    }
}

impl LogLike for subscriptions::build_logs::LogFields {
    fn message(&self) -> &str {
        &self.message
    }
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn attributes(&self) -> Vec<(&str, &str)> {
        self.attributes
            .iter()
            .map(|a| (a.key.as_str(), a.value.as_str()))
            .collect()
    }
}

impl LogLike for queries::build_logs::LogFields {
    fn message(&self) -> &str {
        &self.message
    }
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn attributes(&self) -> Vec<(&str, &str)> {
        self.attributes
            .iter()
            .map(|a| (a.key.as_str(), a.value.as_str()))
            .collect()
    }
}

impl HttpLogLike for queries::http_logs::HttpLogFields {
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn method(&self) -> &str {
        &self.method
    }
    fn path(&self) -> &str {
        &self.path
    }
    fn http_status(&self) -> i64 {
        self.http_status
    }
    fn total_duration(&self) -> i64 {
        self.total_duration
    }
    fn request_id(&self) -> &str {
        &self.request_id
    }
}

impl HttpLogLike for subscriptions::http_logs::HttpLogFields {
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn method(&self) -> &str {
        &self.method
    }
    fn path(&self) -> &str {
        &self.path
    }
    fn http_status(&self) -> i64 {
        self.http_status
    }
    fn total_duration(&self) -> i64 {
        self.total_duration
    }
    fn request_id(&self) -> &str {
        &self.request_id
    }
}

impl NetworkFlowLogLike for queries::network_flow_logs::NetworkFlowLogFields {
    fn capture_end(&self) -> &str {
        &self.capture_end
    }

    fn direction_value(&self) -> String {
        serialized_enum_value(&self.direction)
    }

    fn l4_protocol_value(&self) -> String {
        serialized_enum_value(&self.l4_protocol)
    }

    fn src_addr(&self) -> &str {
        &self.src_addr
    }

    fn src_port(&self) -> i64 {
        self.src_port
    }

    fn dst_addr(&self) -> &str {
        &self.dst_addr
    }

    fn dst_port(&self) -> i64 {
        self.dst_port
    }

    fn peer_kind_value(&self) -> String {
        serialized_enum_value(&self.peer_kind)
    }

    fn byte_count(&self) -> i64 {
        self.byte_count
    }

    fn l4_latency_ms(&self) -> f64 {
        self.l4_latency_ms
    }

    fn drop_cause(&self) -> Option<&str> {
        self.drop_cause.as_deref()
    }
}

impl NetworkFlowLogLike for subscriptions::network_flow_logs::NetworkFlowLogFields {
    fn capture_end(&self) -> &str {
        &self.capture_end
    }

    fn direction_value(&self) -> String {
        serialized_enum_value(&self.direction)
    }

    fn l4_protocol_value(&self) -> String {
        serialized_enum_value(&self.l4_protocol)
    }

    fn src_addr(&self) -> &str {
        &self.src_addr
    }

    fn src_port(&self) -> i64 {
        self.src_port
    }

    fn dst_addr(&self) -> &str {
        &self.dst_addr
    }

    fn dst_port(&self) -> i64 {
        self.dst_port
    }

    fn peer_kind_value(&self) -> String {
        serialized_enum_value(&self.peer_kind)
    }

    fn byte_count(&self) -> i64 {
        self.byte_count
    }

    fn l4_latency_ms(&self) -> f64 {
        self.l4_latency_ms
    }

    fn drop_cause(&self) -> Option<&str> {
        self.drop_cause.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test struct that implements LogLike for testing
    #[derive(serde::Serialize)]
    struct TestLog {
        message: String,
        timestamp: String,
        attributes: Vec<(String, String)>,
    }

    impl LogLike for TestLog {
        fn message(&self) -> &str {
            &self.message
        }
        fn timestamp(&self) -> &str {
            &self.timestamp
        }
        fn attributes(&self) -> Vec<(&str, &str)> {
            self.attributes
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect()
        }
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct TestNetworkFlowLog {
        flow_id: String,
        capture_start: String,
        capture_end: String,
        flow_state: String,
        direction: String,
        l4_protocol: String,
        src_addr: String,
        src_port: i64,
        dst_addr: String,
        dst_port: i64,
        peer_kind: String,
        peer_service_id: Option<String>,
        byte_count: i64,
        packet_count: i64,
        l4_latency_ms: f64,
        drop_cause: Option<String>,
        service_id: String,
        deployment_id: String,
        deployment_instance_id: String,
    }

    impl TestNetworkFlowLog {
        fn example() -> Self {
            Self {
                flow_id: "flow-123".to_string(),
                capture_start: "2026-06-16T00:15:14.000Z".to_string(),
                capture_end: "2026-06-16T00:15:14.000Z".to_string(),
                flow_state: "complete".to_string(),
                direction: "ingress".to_string(),
                l4_protocol: "tcp".to_string(),
                src_addr: "10.202.164.239".to_string(),
                src_port: 8080,
                dst_addr: "100.64.0.2".to_string(),
                dst_port: 51222,
                peer_kind: "internet".to_string(),
                peer_service_id: None,
                byte_count: 418,
                packet_count: 6,
                l4_latency_ms: 0.0,
                drop_cause: None,
                service_id: "service-123".to_string(),
                deployment_id: "deployment-123".to_string(),
                deployment_instance_id: "instance-123".to_string(),
            }
        }
    }

    impl NetworkFlowLogLike for TestNetworkFlowLog {
        fn capture_end(&self) -> &str {
            &self.capture_end
        }

        fn direction_value(&self) -> String {
            self.direction.clone()
        }

        fn l4_protocol_value(&self) -> String {
            self.l4_protocol.clone()
        }

        fn src_addr(&self) -> &str {
            &self.src_addr
        }

        fn src_port(&self) -> i64 {
            self.src_port
        }

        fn dst_addr(&self) -> &str {
            &self.dst_addr
        }

        fn dst_port(&self) -> i64 {
            self.dst_port
        }

        fn peer_kind_value(&self) -> String {
            self.peer_kind.clone()
        }

        fn byte_count(&self) -> i64 {
            self.byte_count
        }

        fn l4_latency_ms(&self) -> f64 {
            self.l4_latency_ms
        }

        fn drop_cause(&self) -> Option<&str> {
            self.drop_cause.as_deref()
        }
    }

    #[test]
    fn test_format_attr_log_no_attributes() {
        let log = TestLog {
            message: "Test message".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            attributes: vec![],
        };

        // Should only return the message
        let output = format_attr_log_string(&log, false);
        assert_eq!(output, "Test message");
    }

    #[test]
    fn test_format_network_flow_log_json_adds_timestamp_alias() {
        let output = format_network_flow_log_string(&TestNetworkFlowLog::example(), true);
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(value["timestamp"], "2026-06-16T00:15:14.000Z");
        assert_eq!(value["flowId"], "flow-123");
        assert_eq!(value["captureEnd"], "2026-06-16T00:15:14.000Z");
        assert_eq!(value["dropCause"], serde_json::Value::Null);
        assert!(value.get("status").is_none());
    }

    #[test]
    fn test_format_network_flow_log_table_row() {
        let header = format_network_flow_log_header();
        let row = format_network_flow_log_string(&TestNetworkFlowLog::example(), false);

        assert!(header.contains("Time"));
        assert!(header.contains("Traffic"));
        assert!(header.contains("Status"));
        assert!(row.contains("10.202.164.239:8080"));
        assert!(row.contains("100.64.0.2:51222"));
        assert!(row.contains("Internet"));
        assert!(row.contains("418 B"));
        assert!(row.contains("0ms"));
        assert!(row.contains("OK"));
    }

    #[test]
    fn test_format_network_flow_log_brackets_ipv6_endpoints() {
        let mut log = TestNetworkFlowLog::example();
        log.src_addr = "fd12:f783:b81d:1:b000:23:a80e:1d89".to_string();
        log.dst_addr = "fd12:f783:b81d:1:b000:a4:5366:c08c".to_string();

        let row = format_network_flow_log_string(&log, false);

        assert!(row.contains("[fd12:f783:b81d:1:b000:23:a80e:1d89]:8080"));
        assert!(row.contains("[fd12:f783:b81d:1:b000:a4:5366:c08c]:51222"));
    }

    #[test]
    fn test_network_flow_direction_labels_match_dashboard_orientation() {
        assert_eq!(direction_label_for_terminal("ingress", false), "↓");
        assert_eq!(direction_label_for_terminal("egress", false), "↑");
        assert_eq!(direction_label_for_terminal("ingress", true), "in");
        assert_eq!(direction_label_for_terminal("egress", true), "out");
    }

    #[test]
    fn test_format_attr_log_only_level() {
        let log = TestLog {
            message: "Test message".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            attributes: vec![("level".to_string(), "info".to_string())],
        };

        // Should only return message when only attribute is level
        let output = format_attr_log_string(&log, false);
        assert_eq!(output, "Test message");
    }

    #[test]
    fn test_format_attr_log_with_attributes_level_only() {
        let log = TestLog {
            message: "Test message".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            attributes: vec![
                ("level".to_string(), "error".to_string()),
                ("service".to_string(), "api".to_string()),
                ("replica".to_string(), "xyz123".to_string()),
            ],
        };

        // With show_all_attributes=false, should only show level + message
        let output = format_attr_log_string(&log, false);
        assert!(output.contains("Test message"));
        // Should NOT contain the extra attributes
        assert!(!output.contains("service"));
        assert!(!output.contains("api"));
    }

    #[test]
    fn test_format_attr_log_with_attributes_full() {
        let log = TestLog {
            message: "Test message".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            attributes: vec![
                ("level".to_string(), "error".to_string()),
                ("service".to_string(), "api".to_string()),
                ("replica".to_string(), "xyz123".to_string()),
            ],
        };

        // With show_all_attributes=true, should format with all attributes
        let output = format_attr_log_string(&log, true);
        assert!(output.contains("Test message"));
        assert!(output.contains("2025-01-01T00:00:00Z"));
        assert!(output.contains("service"));
        assert!(output.contains("api"));
        assert!(output.contains("replica"));
        assert!(output.contains("xyz123"));
    }

    #[test]
    fn test_print_log_json_mode() {
        let log = TestLog {
            message: "Test message".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            attributes: vec![
                ("level".to_string(), "warn".to_string()),
                ("count".to_string(), "42".to_string()),
            ],
        };

        // Test JSON output mode (format param is ignored for JSON)
        let output = format_log_string(log, true, LogFormat::Full);
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["message"], "Test message");
        assert_eq!(json["timestamp"], "2025-01-01T00:00:00Z");
        assert_eq!(json["level"], "warn");
        assert_eq!(json["count"], 42); // This parses as a number
    }
}
