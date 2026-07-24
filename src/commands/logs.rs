use std::fmt;
use std::str::FromStr;

use is_terminal::IsTerminal;

use crate::{
    controllers::{
        deployment::{
            FetchDnsQueryLogsParams, FetchLogsParams, FetchNetworkFlowLogsParams, fetch_build_logs,
            fetch_deploy_logs, fetch_dns_query_logs, fetch_http_logs, fetch_network_flow_logs,
            stream_build_logs, stream_deploy_logs, stream_dns_query_logs, stream_http_logs,
            stream_network_flow_logs,
        },
        project::resolve_service_context,
    },
    util::{
        logs::{
            LogFormat, format_dns_query_log_header, format_network_flow_log_header,
            print_dns_query_log, print_http_log, print_log, print_network_flow_log,
        },
        time::parse_time,
    },
};
use anyhow::{Context, bail};

use super::{
    queries::deployments::{DeploymentListInput, DeploymentStatus},
    *,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum HttpMethod {
    #[value(name = "GET")]
    Get,
    #[value(name = "POST")]
    Post,
    #[value(name = "PUT")]
    Put,
    #[value(name = "DELETE")]
    Delete,
    #[value(name = "PATCH")]
    Patch,
    #[value(name = "HEAD")]
    Head,
    #[value(name = "OPTIONS")]
    Options,
}

impl fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Get => write!(f, "GET"),
            Self::Post => write!(f, "POST"),
            Self::Put => write!(f, "PUT"),
            Self::Delete => write!(f, "DELETE"),
            Self::Patch => write!(f, "PATCH"),
            Self::Head => write!(f, "HEAD"),
            Self::Options => write!(f, "OPTIONS"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum StatusFilter {
    Exact(u16),
    Comparison { op: &'static str, value: u16 },
    Range { low: u16, high: u16 },
}

impl StatusFilter {
    fn to_filter_expr(&self) -> String {
        match self {
            Self::Exact(v) => format!("@httpStatus:{v}"),
            Self::Comparison { op, value } => format!("@httpStatus:{op}{value}"),
            Self::Range { low, high } => format!("@httpStatus:{low}..{high}"),
        }
    }
}

impl FromStr for StatusFilter {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Try range first: "200..299"
        if let Some((low, high)) = s.split_once("..") {
            let low: u16 = low
                .parse()
                .map_err(|_| format!("Invalid status range start: {low}"))?;
            let high: u16 = high
                .parse()
                .map_err(|_| format!("Invalid status range end: {high}"))?;
            if low > high {
                return Err(format!("Range start ({low}) must be <= end ({high})"));
            }
            return Ok(Self::Range { low, high });
        }

        // Try comparison: ">=400", ">399", "<=499", "<500"
        for prefix in &[">=", "<=", ">", "<"] {
            if let Some(rest) = s.strip_prefix(prefix) {
                let value: u16 = rest
                    .parse()
                    .map_err(|_| format!("Invalid status code: {rest}"))?;
                return Ok(Self::Comparison { op: prefix, value });
            }
        }

        // Exact: "200"
        let value: u16 = s.parse().map_err(|_| {
            format!(
                "Invalid status filter: \"{s}\". Expected a number (200), comparison (>=400), or range (500..599)"
            )
        })?;
        Ok(Self::Exact(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum NetworkFlowProtocol {
    Tcp,
    Udp,
    Icmp,
    Icmpv6,
    Unknown,
}

impl fmt::Display for NetworkFlowProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp => write!(f, "tcp"),
            Self::Udp => write!(f, "udp"),
            Self::Icmp => write!(f, "icmp"),
            Self::Icmpv6 => write!(f, "icmpv6"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum NetworkFlowDirection {
    Ingress,
    Egress,
}

impl fmt::Display for NetworkFlowDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ingress => write!(f, "ingress"),
            Self::Egress => write!(f, "egress"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum NetworkFlowPeerKind {
    Service,
    Internet,
    #[value(name = "edge_proxy", alias = "edge-proxy")]
    EdgeProxy,
    #[value(name = "local_dns", alias = "dns")]
    LocalDns,
    Unknown,
}

impl fmt::Display for NetworkFlowPeerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Service => write!(f, "service"),
            Self::Internet => write!(f, "internet"),
            Self::EdgeProxy => write!(f, "edge_proxy"),
            Self::LocalDns => write!(f, "local_dns"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DnsRecordType {
    #[value(name = "A")]
    A,
    #[value(name = "AAAA")]
    Aaaa,
    #[value(name = "CNAME")]
    Cname,
    #[value(name = "TXT")]
    Txt,
    #[value(name = "MX")]
    Mx,
    #[value(name = "SRV")]
    Srv,
    #[value(name = "NS")]
    Ns,
}

impl fmt::Display for DnsRecordType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::A => write!(f, "A"),
            Self::Aaaa => write!(f, "AAAA"),
            Self::Cname => write!(f, "CNAME"),
            Self::Txt => write!(f, "TXT"),
            Self::Mx => write!(f, "MX"),
            Self::Srv => write!(f, "SRV"),
            Self::Ns => write!(f, "NS"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DnsResponseCode {
    #[value(name = "NOERROR")]
    NoError,
    #[value(name = "NXDOMAIN")]
    Nxdomain,
    #[value(name = "SERVFAIL")]
    Servfail,
    #[value(name = "REFUSED")]
    Refused,
    #[value(name = "TIMEOUT")]
    Timeout,
    #[value(name = "ERROR")]
    Error,
}

impl fmt::Display for DnsResponseCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoError => write!(f, "NOERROR"),
            Self::Nxdomain => write!(f, "NXDOMAIN"),
            Self::Servfail => write!(f, "SERVFAIL"),
            Self::Refused => write!(f, "REFUSED"),
            Self::Timeout => write!(f, "TIMEOUT"),
            Self::Error => write!(f, "ERROR"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DnsZone {
    Internal,
    External,
}

impl fmt::Display for DnsZone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Internal => write!(f, "internal"),
            Self::External => write!(f, "external"),
        }
    }
}

fn build_http_filter(args: &Args) -> Result<Option<String>> {
    let status = args
        .status
        .as_deref()
        .map(StatusFilter::from_str)
        .transpose()
        .map_err(anyhow::Error::msg)?;

    Ok(compose_http_filter(
        args.method.as_ref(),
        status.as_ref(),
        args.path.as_deref(),
        args.request_id.as_deref(),
        args.filter.as_deref(),
    ))
}

fn build_dns_filter(args: &Args) -> Result<Option<String>> {
    let mut filters = Vec::new();

    if let Some(domain) = args.domain.as_deref() {
        filters.push(format!("@domain:{domain}"));
    }
    if let Some(qname) = args.qname.as_deref() {
        filters.push(format!("@qname:{qname}"));
    }
    if let Some(qtype) = args.qtype {
        filters.push(format!("@qtype:{qtype}"));
    }
    if let Some(rcode) = args.rcode {
        filters.push(format!("@rcode:{rcode}"));
    }
    if let Some(zone) = args.zone {
        filters.push(format!("@zone:{zone}"));
    }
    if let Some(status) = args.status.as_deref() {
        let status = status.to_ascii_lowercase();
        if status != "ok" && status != "failed" {
            bail!("Invalid DNS status: {status}. Expected ok or failed.");
        }
        filters.push(format!("@status:{status}"));
    }
    if let Some(raw_filter) = args.filter.as_deref().filter(|filter| !filter.is_empty()) {
        filters.push(raw_filter.to_string());
    }

    Ok((!filters.is_empty()).then(|| filters.join(" ")))
}

fn build_network_flow_filter(args: &Args) -> Result<Option<String>> {
    if args.method.is_some() || args.path.is_some() || args.request_id.is_some() {
        bail!("--method, --path, and --request-id can only be used with --http");
    }

    compose_network_flow_filter(NetworkFlowFilterParts {
        protocol: args.protocol.as_ref(),
        direction: args.direction.as_ref(),
        peer: args.peer.as_deref(),
        peer_kind: args.peer_kind.as_ref(),
        status: args.status.as_deref(),
        dropped: args.dropped,
        port: args.port,
        src: args.src.as_deref(),
        dst: args.dst.as_deref(),
        host: args.host.as_deref(),
        drop_cause: args.drop_cause.as_deref(),
        raw_filter: args.filter.as_deref(),
    })
}

fn validate_filter_modes(args: &Args) -> Result<()> {
    if args.status.is_some() && !args.http && !args.network && !args.dns {
        bail!("--status can only be used with --http, --network, or --dns");
    }

    if !args.http && (args.method.is_some() || args.path.is_some() || args.request_id.is_some()) {
        bail!("--method, --path, and --request-id can only be used with --http");
    }

    if !args.network && has_network_flow_filter_args(args) {
        bail!(
            "--protocol, --direction, --peer, --peer-kind, --dropped, --port, --src, --dst, --host, and --drop-cause can only be used with --network"
        );
    }

    if !args.dns && has_dns_filter_args(args) {
        bail!("--domain, --qname, --qtype, --rcode, and --zone can only be used with --dns");
    }

    Ok(())
}

fn has_dns_filter_args(args: &Args) -> bool {
    args.domain.is_some()
        || args.qname.is_some()
        || args.qtype.is_some()
        || args.rcode.is_some()
        || args.zone.is_some()
}

fn has_network_flow_filter_args(args: &Args) -> bool {
    args.protocol.is_some()
        || args.direction.is_some()
        || args.peer.is_some()
        || args.peer_kind.is_some()
        || args.dropped.is_some()
        || args.port.is_some()
        || args.src.is_some()
        || args.dst.is_some()
        || args.host.is_some()
        || args.drop_cause.is_some()
}

pub struct NetworkFlowFilterParts<'a> {
    pub protocol: Option<&'a NetworkFlowProtocol>,
    pub direction: Option<&'a NetworkFlowDirection>,
    pub peer: Option<&'a str>,
    pub peer_kind: Option<&'a NetworkFlowPeerKind>,
    pub status: Option<&'a str>,
    pub dropped: Option<bool>,
    pub port: Option<u16>,
    pub src: Option<&'a str>,
    pub dst: Option<&'a str>,
    pub host: Option<&'a str>,
    pub drop_cause: Option<&'a str>,
    pub raw_filter: Option<&'a str>,
}

pub fn compose_network_flow_filter(parts: NetworkFlowFilterParts<'_>) -> Result<Option<String>> {
    let mut filters = Vec::new();

    if let Some(protocol) = parts.protocol {
        filters.push(format!("@protocol:{protocol}"));
    }
    if let Some(direction) = parts.direction {
        filters.push(format!("@direction:{direction}"));
    }
    if let Some(peer) = parts.peer {
        filters.push(peer_filter(peer));
    }
    if let Some(peer_kind) = parts.peer_kind {
        filters.push(format!("@peer_kind:{peer_kind}"));
    }
    if let Some(status) = parts.status {
        let status = status.to_ascii_lowercase();
        if status != "ok" && status != "dropped" {
            bail!("Invalid network flow status: {status}. Expected ok or dropped.");
        }
        filters.push(format!("@status:{status}"));
    }
    if let Some(dropped) = parts.dropped {
        filters.push(format!("@dropped:{dropped}"));
    }
    if let Some(port) = parts.port {
        filters.push(format!("@port:{port}"));
    }
    if let Some(src) = parts.src {
        filters.push(format!("@src:{src}"));
    }
    if let Some(dst) = parts.dst {
        filters.push(format!("@dst:{dst}"));
    }
    if let Some(host) = parts.host {
        filters.push(format!("@host:{host}"));
    }
    if let Some(drop_cause) = parts.drop_cause {
        filters.push(format!("@drop_cause:{drop_cause}"));
    }
    if let Some(raw_filter) = parts.raw_filter.filter(|filter| !filter.is_empty()) {
        filters.push(raw_filter.to_string());
    }

    Ok((!filters.is_empty()).then(|| filters.join(" ")))
}

fn peer_filter(peer: &str) -> String {
    match peer.to_ascii_lowercase().as_str() {
        "internet" => "@peer_kind:internet".to_string(),
        "dns" | "local_dns" | "local-dns" => "@peer_kind:local_dns".to_string(),
        "edge-proxy" | "edge_proxy" => "@peer_kind:edge_proxy".to_string(),
        _ if peer_is_uuid(peer) => format!("@peer:{peer}"),
        _ => format!("@peer:{}", serde_json::to_string(peer).unwrap()),
    }
}

fn peer_is_uuid(peer: &str) -> bool {
    peer.len() == 36 && peer.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-')
}

fn parse_network_port(value: &str) -> std::result::Result<u16, String> {
    let port = value
        .parse::<u16>()
        .map_err(|_| "port must be a number from 1 to 65535".to_string())?;

    if port == 0 {
        return Err("port must be a number from 1 to 65535".to_string());
    }

    Ok(port)
}

/// Build a Railway query filter string from typed HTTP filter components.
pub fn compose_http_filter(
    method: Option<&HttpMethod>,
    status: Option<&StatusFilter>,
    path: Option<&str>,
    request_id: Option<&str>,
    raw_filter: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    if let Some(method) = method {
        parts.push(format!("@method:{method}"));
    }

    if let Some(status) = status {
        parts.push(status.to_filter_expr());
    }

    if let Some(path) = path {
        parts.push(format!("@path:{path}"));
    }

    if let Some(request_id) = request_id {
        parts.push(format!("@requestId:{request_id}"));
    }

    if let Some(raw_filter) = raw_filter {
        if !raw_filter.is_empty() {
            parts.push(raw_filter.to_string());
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

#[derive(Parser)]
#[clap(
    about = "View build, deploy, HTTP, network flow, or DNS logs",
    long_about = "View build, deploy, HTTP, network flow, or DNS logs. This will stream logs by default, or fetch historical logs if the --lines, --since, or --until flags are provided.",
    after_help = "Examples:

  Deployment logs:
  railway logs                                                       # Stream live logs from latest deployment
  railway logs --build 7422c95b-c604-46bc-9de4-b7a43e1fd53d          # Stream build logs from a specific deployment
  railway logs --lines 100                                           # Pull last 100 logs without streaming
  railway logs --since 1h                                            # View logs from the last hour
  railway logs --since 30m --until 10m                               # View logs from 30 minutes ago until 10 minutes ago
  railway logs --since 2024-01-15T10:00:00Z                          # View logs since a specific timestamp
  railway logs --service backend --environment production            # Stream logs from a specific service/environment
  railway logs --lines 10 --filter \"@level:error\"                    # View 10 latest error logs
  railway logs --lines 10 --filter \"@level:warn AND rate limit\"      # View 10 latest warning logs related to rate limiting
  railway logs --json                                                # Get logs in JSON format
  railway logs --latest                                              # Stream logs from the latest deployment (even if failed/building)

  HTTP logs (typed filters):
  railway logs --http --method GET --status 200                      # GET requests with 200 status
  railway logs --http --method POST --path /api/users                # POST requests to /api/users
  railway logs --http --status \">=400\" --lines 50                    # Client/server errors, last 50
  railway logs --http --status 500..599                              # Server errors only
  railway logs --http --request-id abc123                            # Find a specific request

  HTTP logs (raw filter for advanced queries):
  railway logs --http --method GET --filter \"@totalDuration:>=1000\"  # Slow GET requests (combining typed + raw)
  railway logs --http --filter \"@srcIp:203.0.113.1 @edgeRegion:us-east-1\"  # Filter by source IP and region
  railway logs --http --filter \"@httpStatus:>=400 AND @path:/api\"   # Errors on API routes
  railway logs --http --filter \"-@method:OPTIONS\"                    # Exclude OPTIONS requests

  Network flow logs:
  railway logs --network                                              # Stream live network flows
  railway logs --network --lines 100                                  # Pull a snapshot and exit
  railway logs --network --direction egress --protocol tcp            # Outbound TCP flows
  railway logs --network --peer postgres --port 5432                  # Flows to a service peer
  railway logs --network --status dropped                             # Dropped flows

  DNS logs:
  railway logs --dns                                                  # Stream live DNS queries
  railway logs --dns --status failed                                  # Failed lookups only
  railway logs --dns --rcode NXDOMAIN                                 # Names that don't exist
  railway logs --dns --zone internal                                  # Private network lookups
  railway logs --dns --domain example.com --qtype AAAA                # IPv6 lookups for a domain
  railway logs --dns --qname backend.railway.internal --lines 50      # History for an exact name"
)]
pub struct Args {
    /// Service to view logs from (defaults to linked service). Can be service name or service ID
    #[clap(short, long)]
    service: Option<String>,

    /// Environment to view logs from (defaults to linked environment). Can be environment name or environment ID
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

    /// Show deployment logs
    #[clap(short, long, group = "log_type")]
    deployment: bool,

    /// Show build logs
    #[clap(short, long, group = "log_type")]
    build: bool,

    /// Show HTTP request logs
    #[clap(long, group = "log_type")]
    http: bool,

    /// Show network flow logs
    #[clap(long, group = "log_type")]
    network: bool,

    /// Show DNS query logs
    #[clap(long, group = "log_type")]
    dns: bool,

    /// Deployment ID to view logs from. Defaults to most recent successful deployment, or latest deployment if none succeeded
    deployment_id: Option<String>,

    /// Output logs in JSON format. Each log line becomes a JSON object with timestamp, message, and any other attributes
    #[clap(long)]
    json: bool,

    /// Number of log lines to fetch (disables streaming)
    #[clap(short = 'n', long = "lines", visible_alias = "tail")]
    lines: Option<i64>,

    /// Filter logs using Railway's query syntax
    #[clap(
        long,
        short = 'f',
        long_help = "\
Filter logs using Railway's query syntax

For deploy/build logs:
  Text search:   \"error message\", \"user signup\"
  Level filter:  @level:error, @level:warn, @level:info

For HTTP logs (--http), all filterable fields:
  String:  @method, @path, @host, @requestId, @clientUa, @srcIp,
           @edgeRegion, @upstreamAddress, @upstreamProto,
           @downstreamProto, @responseDetails,
           @deploymentId, @deploymentInstanceId
  Numeric: @httpStatus, @totalDuration, @responseTime,
           @upstreamRqDuration, @txBytes, @rxBytes, @upstreamErrors

For network flow logs (--network), all filterable fields:
  String:  @protocol, @direction, @peer, @peer_kind, @status,
           @drop_cause, @src, @dst, @host
  Numeric: @port
  Boolean: @dropped

For DNS logs (--dns), all filterable fields:
  String:  @qname, @domain, @qtype, @rcode, @zone, @status

Numeric operators: > >= < <= .. (range, e.g. @httpStatus:200..299)
Logical operators: AND, OR, - (negation), parentheses for grouping

Examples:
  @httpStatus:>=400
  @totalDuration:>1000
  -@method:OPTIONS
  @httpStatus:>=400 AND @path:/api
  (@method:GET OR @method:POST) AND @httpStatus:500
  @protocol:tcp @direction:egress
  @peer_kind:internet @port:443"
    )]
    filter: Option<String>,

    /// Filter HTTP logs by request method (requires --http)
    #[clap(long, requires = "http", value_enum, ignore_case = true)]
    method: Option<HttpMethod>,

    /// Filter HTTP logs by status code, network flow logs by status (ok, dropped), or DNS logs by resolution status (ok, failed)
    #[clap(long, value_name = "STATUS")]
    status: Option<String>,

    /// Filter HTTP logs by request path (requires --http)
    #[clap(long, requires = "http", value_name = "PATH")]
    path: Option<String>,

    /// Filter HTTP logs by request ID (requires --http)
    #[clap(long = "request-id", requires = "http", value_name = "ID")]
    request_id: Option<String>,

    /// Filter network flow logs by layer 4 protocol
    #[clap(long, requires = "network", value_enum, ignore_case = true)]
    protocol: Option<NetworkFlowProtocol>,

    /// Filter network flow logs by traffic direction
    #[clap(long, requires = "network", value_enum, ignore_case = true)]
    direction: Option<NetworkFlowDirection>,

    /// Filter network flow logs by peer service name/ID or a well-known peer (internet, dns, edge-proxy)
    #[clap(long, requires = "network", value_name = "PEER")]
    peer: Option<String>,

    /// Filter network flow logs by peer kind
    #[clap(
        long = "peer-kind",
        requires = "network",
        value_enum,
        ignore_case = true
    )]
    peer_kind: Option<NetworkFlowPeerKind>,

    /// Filter network flow logs by whether packets were dropped
    #[clap(long, requires = "network", value_name = "BOOL")]
    dropped: Option<bool>,

    /// Filter network flow logs by source or destination port
    #[clap(long, requires = "network", value_parser = parse_network_port)]
    port: Option<u16>,

    /// Filter network flow logs by source IP
    #[clap(long, requires = "network", value_name = "IP")]
    src: Option<String>,

    /// Filter network flow logs by destination IP
    #[clap(long, requires = "network", value_name = "IP")]
    dst: Option<String>,

    /// Filter network flow logs by source or destination IP
    #[clap(long, requires = "network", value_name = "IP")]
    host: Option<String>,

    /// Filter network flow logs by drop cause
    #[clap(long = "drop-cause", requires = "network", value_name = "CAUSE")]
    drop_cause: Option<String>,

    /// Filter DNS logs by domain, including its subdomains
    #[clap(long, requires = "dns", value_name = "DOMAIN")]
    domain: Option<String>,

    /// Filter DNS logs by the exact name looked up
    #[clap(long, requires = "dns", value_name = "NAME")]
    qname: Option<String>,

    /// Filter DNS logs by record type
    #[clap(long, requires = "dns", value_enum, ignore_case = true)]
    qtype: Option<DnsRecordType>,

    /// Filter DNS logs by response code
    #[clap(long, requires = "dns", value_enum, ignore_case = true)]
    rcode: Option<DnsResponseCode>,

    /// Filter DNS logs by lookup zone
    #[clap(long, requires = "dns", value_enum, ignore_case = true)]
    zone: Option<DnsZone>,

    /// Always show logs from the latest deployment, even if it failed or is still building
    #[clap(long)]
    latest: bool,

    /// Show logs since a specific time (disables streaming). Accepts relative times (e.g., 30s, 5m, 2h, 1d, 1w) or ISO 8601 timestamps (e.g., 2024-01-15T10:30:00Z)
    #[clap(long, short = 'S', value_name = "TIME")]
    since: Option<String>,

    /// Show logs until a specific time (disables streaming). Same formats as --since
    #[clap(long, short = 'U', value_name = "TIME")]
    until: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    validate_filter_modes(&args)?;

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let backboard = configs.get_backboard();

    // Build filter before args is partially moved by service matching below
    let http_filter = if args.http {
        build_http_filter(&args)?
    } else {
        None
    };
    let network_filter = if args.network {
        build_network_flow_filter(&args)?
    } else {
        None
    };
    let dns_filter = if args.dns {
        build_dns_filter(&args)?
    } else {
        None
    };

    let start_date = args.since.as_ref().map(|s| parse_time(s)).transpose()?;
    let end_date = args.until.as_ref().map(|s| parse_time(s)).transpose()?;

    if let (Some(s), Some(e)) = (&start_date, &end_date) {
        if s >= e {
            bail!("--since time must be before --until time");
        }
    }

    let has_time_filter = start_date.is_some() || end_date.is_some();

    // Stream only if no line limit or time filter is specified and running in a terminal
    let should_stream = args.lines.is_none() && !has_time_filter && std::io::stdout().is_terminal();

    let ctx = resolve_service_context(args.project, args.service, args.environment).await?;
    let project_id = ctx.project_id;
    let environment_id = ctx.environment_id;
    let service = ctx.service_id;

    if args.network {
        if args.deployment_id.is_some() || args.latest {
            bail!(
                "deployment IDs and --latest can only be used with deployment, build, or HTTP logs"
            );
        }

        if !args.json {
            println!("{}", format_network_flow_log_header());
        }

        if should_stream {
            stream_network_flow_logs(environment_id, Some(service), network_filter, |log| {
                print_network_flow_log(log, args.json)
            })
            .await?;
        } else {
            let mut has_logs = false;
            fetch_network_flow_logs(
                FetchNetworkFlowLogsParams {
                    client: &client,
                    backboard: &backboard,
                    environment_id,
                    service_id: Some(service),
                    limit: args.lines.or(Some(500)),
                    filter: network_filter,
                    start_date,
                    end_date,
                },
                |log| {
                    has_logs = true;
                    print_network_flow_log(log, args.json)
                },
            )
            .await?;

            if !has_logs && !args.json {
                println!("No network flows found");
            }
        }

        return Ok(());
    }

    if args.dns {
        if args.deployment_id.is_some() || args.latest {
            bail!(
                "deployment IDs and --latest can only be used with deployment, build, or HTTP logs"
            );
        }

        if !args.json {
            println!("{}", format_dns_query_log_header());
        }

        if should_stream {
            stream_dns_query_logs(environment_id, Some(service), dns_filter, |log| {
                print_dns_query_log(log, args.json)
            })
            .await?;
        } else {
            let mut has_logs = false;
            fetch_dns_query_logs(
                FetchDnsQueryLogsParams {
                    client: &client,
                    backboard: &backboard,
                    environment_id,
                    service_id: Some(service),
                    limit: args.lines.or(Some(500)),
                    filter: dns_filter,
                    start_date,
                    end_date,
                },
                |log| {
                    has_logs = true;
                    print_dns_query_log(log, args.json)
                },
            )
            .await?;

            if !has_logs && !args.json {
                println!("No DNS queries found");
            }
        }

        return Ok(());
    }

    // Fetch all deployments so we can find a sensible default deployment id if
    // none is provided
    let vars = queries::deployments::Variables {
        input: DeploymentListInput {
            project_id: Some(project_id.clone()),
            environment_id: Some(environment_id),
            service_id: Some(service),
            include_deleted: None,
            status: None,
        },
        first: None,
    };
    let deployments = post_graphql::<queries::Deployments, _>(&client, &backboard, vars)
        .await?
        .deployments;
    let mut all_deployments: Vec<_> = deployments
        .edges
        .into_iter()
        .map(|deployment| deployment.node)
        .collect();
    all_deployments.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let default_deployment = if args.latest {
        all_deployments.first()
    } else {
        all_deployments
            .iter()
            .find(|d| d.status == DeploymentStatus::SUCCESS)
            .or_else(|| all_deployments.first())
    }
    .context("No deployments found")?;

    let deployment_id = if let Some(deployment_id) = args.deployment_id {
        // Use the provided deployment ID directly
        deployment_id
    } else {
        default_deployment.id.clone()
    };

    let show_http_logs = args.http;
    let show_build_logs = !show_http_logs
        && !args.deployment
        && (args.build
            || (default_deployment.status == DeploymentStatus::FAILED
                && deployment_id == default_deployment.id));

    if show_http_logs {
        if should_stream {
            stream_http_logs(deployment_id.clone(), http_filter, |log| {
                print_http_log(log, args.json)
            })
            .await?;
        } else {
            fetch_http_logs(
                FetchLogsParams {
                    client: &client,
                    backboard: &backboard,
                    deployment_id: deployment_id.clone(),
                    limit: args.lines.or(Some(500)),
                    filter: http_filter,
                    start_date,
                    end_date,
                },
                |log| print_http_log(log, args.json),
            )
            .await?;
        }
    } else if show_build_logs {
        if should_stream {
            stream_build_logs(deployment_id.clone(), args.filter.clone(), |log| {
                print_log(log, args.json, LogFormat::LevelOnly)
            })
            .await?;
        } else {
            fetch_build_logs(
                FetchLogsParams {
                    client: &client,
                    backboard: &backboard,
                    deployment_id: deployment_id.clone(),
                    limit: args.lines.or(Some(500)),
                    filter: args.filter.clone(),
                    start_date,
                    end_date,
                },
                |log| print_log(log, args.json, LogFormat::LevelOnly),
            )
            .await?;
        }
    } else if should_stream {
        stream_deploy_logs(deployment_id.clone(), args.filter.clone(), |log| {
            print_log(log, args.json, LogFormat::Full)
        })
        .await?;
    } else {
        fetch_deploy_logs(
            FetchLogsParams {
                client: &client,
                backboard: &backboard,
                deployment_id: deployment_id.clone(),
                limit: args.lines.or(Some(500)),
                filter: args.filter.clone(),
                start_date,
                end_date,
            },
            |log| print_log(log, args.json, LogFormat::Full),
        )
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_args() -> Args {
        Args {
            service: None,
            environment: None,
            project: None,
            deployment: false,
            build: false,
            http: false,
            network: false,
            dns: false,
            deployment_id: None,
            json: false,
            lines: None,
            filter: None,
            method: None,
            status: None,
            path: None,
            request_id: None,
            protocol: None,
            direction: None,
            peer: None,
            peer_kind: None,
            dropped: None,
            port: None,
            src: None,
            dst: None,
            host: None,
            drop_cause: None,
            domain: None,
            qname: None,
            qtype: None,
            rcode: None,
            zone: None,
            latest: false,
            since: None,
            until: None,
        }
    }

    #[test]
    fn build_http_filter_composes_typed_and_raw() {
        // No filters → None
        let args = Args::parse_from(["logs", "--http"]);
        assert_eq!(build_http_filter(&args).unwrap(), None);

        // All typed flags composed in order
        let args = Args::parse_from([
            "logs",
            "--http",
            "--method",
            "POST",
            "--status",
            ">=400",
            "--path",
            "/api/users",
            "--request-id",
            "abc123",
        ]);
        assert_eq!(
            build_http_filter(&args).unwrap(),
            Some("@method:POST @httpStatus:>=400 @path:/api/users @requestId:abc123".to_string())
        );

        // Typed + raw filter combined
        let args = Args::parse_from([
            "logs",
            "--http",
            "--method",
            "GET",
            "--filter",
            "@totalDuration:>=1000",
        ]);
        assert_eq!(
            build_http_filter(&args).unwrap(),
            Some("@method:GET @totalDuration:>=1000".to_string())
        );
    }

    #[test]
    fn build_network_flow_filter_composes_typed_and_raw() {
        let args = Args::parse_from(["logs", "--network"]);
        assert_eq!(build_network_flow_filter(&args).unwrap(), None);

        let args = Args::parse_from([
            "logs",
            "--network",
            "--protocol",
            "tcp",
            "--direction",
            "egress",
            "--peer",
            "postgres",
            "--status",
            "dropped",
            "--port",
            "5432",
            "--filter",
            "@drop_cause:NO_SOCKET",
        ]);
        assert_eq!(
            build_network_flow_filter(&args).unwrap(),
            Some(
                "@protocol:tcp @direction:egress @peer:\"postgres\" @status:dropped @port:5432 @drop_cause:NO_SOCKET"
                    .to_string()
            )
        );
    }

    #[test]
    fn network_flow_peer_well_known_names_use_peer_kind() {
        let args = Args::parse_from(["logs", "--network", "--peer", "edge-proxy"]);

        assert_eq!(
            build_network_flow_filter(&args).unwrap(),
            Some("@peer_kind:edge_proxy".to_string())
        );
    }

    #[test]
    fn network_flow_logs_are_mutually_exclusive_with_other_log_modes() {
        assert!(Args::try_parse_from(["logs", "--network", "--http"]).is_err());
        assert!(Args::try_parse_from(["logs", "--network", "--build"]).is_err());
        assert!(Args::try_parse_from(["logs", "--network", "--deployment"]).is_err());
    }

    #[test]
    fn build_dns_filter_composes_typed_and_raw() {
        let args = Args::parse_from(["logs", "--dns"]);
        assert_eq!(build_dns_filter(&args).unwrap(), None);

        let args = Args::parse_from([
            "logs",
            "--dns",
            "--domain",
            "example.com",
            "--qname",
            "api.example.com",
            "--qtype",
            "aaaa",
            "--rcode",
            "nxdomain",
            "--zone",
            "external",
            "--status",
            "FAILED",
            "--filter",
            "@answers:10.0.0.1",
        ]);
        assert_eq!(
            build_dns_filter(&args).unwrap(),
            Some(
                "@domain:example.com @qname:api.example.com @qtype:AAAA @rcode:NXDOMAIN @zone:external @status:failed @answers:10.0.0.1"
                    .to_string()
            )
        );
    }

    #[test]
    fn build_dns_filter_rejects_invalid_status() {
        let args = Args::parse_from(["logs", "--dns", "--status", "dropped"]);
        assert!(build_dns_filter(&args).is_err());
    }

    #[test]
    fn dns_logs_are_mutually_exclusive_with_other_log_modes() {
        assert!(Args::try_parse_from(["logs", "--dns", "--http"]).is_err());
        assert!(Args::try_parse_from(["logs", "--dns", "--build"]).is_err());
        assert!(Args::try_parse_from(["logs", "--dns", "--deployment"]).is_err());
        assert!(Args::try_parse_from(["logs", "--dns", "--network"]).is_err());
    }

    #[test]
    fn dns_typed_flags_require_dns_mode() {
        assert!(Args::try_parse_from(["logs", "--rcode", "NXDOMAIN"]).is_err());

        let mut args = base_args();
        args.http = true;
        args.rcode = Some(DnsResponseCode::Nxdomain);
        assert!(validate_filter_modes(&args).is_err());

        let mut args = base_args();
        args.dns = true;
        args.rcode = Some(DnsResponseCode::Nxdomain);
        assert!(validate_filter_modes(&args).is_ok());
    }

    #[test]
    fn status_accepts_dns_mode() {
        let mut args = base_args();
        args.dns = true;
        args.status = Some("failed".to_string());

        assert!(validate_filter_modes(&args).is_ok());
    }

    #[test]
    fn network_flow_typed_flags_require_network_mode() {
        let mut args = base_args();
        args.http = true;
        args.protocol = Some(NetworkFlowProtocol::Tcp);

        assert!(validate_filter_modes(&args).is_err());

        let mut args = base_args();
        args.network = true;
        args.protocol = Some(NetworkFlowProtocol::Tcp);

        assert!(validate_filter_modes(&args).is_ok());
    }

    #[test]
    fn http_typed_flags_require_http_mode() {
        let mut args = base_args();
        args.network = true;
        args.method = Some(HttpMethod::Get);

        assert!(validate_filter_modes(&args).is_err());

        let mut args = base_args();
        args.http = true;
        args.method = Some(HttpMethod::Get);

        assert!(validate_filter_modes(&args).is_ok());
    }

    #[test]
    fn status_requires_http_or_network_mode() {
        let mut args = base_args();
        args.status = Some("ok".to_string());

        assert!(validate_filter_modes(&args).is_err());

        let mut args = base_args();
        args.network = true;
        args.status = Some("ok".to_string());

        assert!(validate_filter_modes(&args).is_ok());
    }
}
