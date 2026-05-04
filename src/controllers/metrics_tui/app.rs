use std::collections::HashMap;

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use futures::FutureExt;

use crate::controllers::db_stats::{self, DatabaseStats};
use crate::controllers::metrics::{
    FetchHttpMetricsParams, FetchProjectMetricsParams, FetchResourceMetricsParams,
    HttpMetricsResult, MetricSummary, ResourceMetricsResult, ServiceMetricsSummary, VolumeMetrics,
    compute_sample_rate, fetch_http_metrics, fetch_project_metrics, fetch_resource_metrics,
    find_metric, get_volume_metrics, is_database_service,
};
use crate::controllers::project::find_service_instance;
use crate::util::time::parse_time;

use tokio::task::JoinHandle;

use super::{POLL_INTERVALS_SECS, ProjectTuiParams, ServiceTuiParams, TIME_RANGES};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    Metrics,
    Stats,
}

type MetricMeasurement = crate::gql::queries::metrics::MetricMeasurement;
type FetchResult<T> = Result<T, String>;

fn build_measurements(
    cpu: bool,
    memory: bool,
    network: bool,
    volume: bool,
) -> Vec<MetricMeasurement> {
    let mut m = Vec::new();
    if cpu {
        m.push(MetricMeasurement::CPU_USAGE);
        m.push(MetricMeasurement::CPU_LIMIT);
    }
    if memory {
        m.push(MetricMeasurement::MEMORY_USAGE_GB);
        m.push(MetricMeasurement::MEMORY_LIMIT_GB);
    }
    if network {
        m.push(MetricMeasurement::NETWORK_TX_GB);
        m.push(MetricMeasurement::NETWORK_RX_GB);
    }
    if volume {
        m.push(MetricMeasurement::DISK_USAGE_GB);
    }
    if m.is_empty() {
        m.push(MetricMeasurement::CPU_USAGE);
    }
    m
}

#[derive(Clone, Copy)]
pub struct ServiceRefreshOptions {
    show_cpu: bool,
    show_memory: bool,
    show_network: bool,
    show_volume: bool,
    show_http: bool,
    is_db: bool,
}

impl ServiceRefreshOptions {
    pub fn from_app(app: &MetricsApp) -> Self {
        Self {
            show_cpu: app.show_cpu,
            show_memory: app.show_memory,
            show_network: app.show_network,
            show_volume: app.show_volume,
            show_http: app.show_http,
            is_db: app.is_db,
        }
    }
}

pub struct ServiceRefreshResult {
    pub request_id: u64,
    pub time_range_idx: usize,
    pub resource: Option<FetchResult<ResourceMetricsResult>>,
    pub http: Option<FetchResult<Option<HttpMetricsResult>>>,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Clone, Copy)]
pub struct ProjectRefreshOptions {
    show_cpu: bool,
    show_memory: bool,
    show_network: bool,
    show_volume: bool,
    show_http: bool,
}

impl ProjectRefreshOptions {
    pub fn from_app(app: &ProjectApp) -> Self {
        Self {
            show_cpu: app.show_cpu,
            show_memory: app.show_memory,
            show_network: app.show_network,
            show_volume: app.show_volume,
            show_http: app.show_http,
        }
    }
}

pub struct ProjectRefreshResult {
    pub request_id: u64,
    pub time_range_idx: usize,
    pub services: FetchResult<Vec<ServiceMetricsSummary>>,
    pub fetched_at: DateTime<Utc>,
}

pub struct ProjectDetailRequest {
    pub request_id: u64,
    pub time_range_idx: usize,
    pub service_id: String,
    pub is_database: bool,
    pub volumes: Vec<VolumeMetrics>,
    pub options: ProjectRefreshOptions,
}

pub struct ProjectDetailResult {
    pub request_id: u64,
    pub service_id: String,
    pub entry: FetchResult<DetailCacheEntry>,
}

pub struct ProjectHttpResult {
    pub request_id: u64,
    pub time_range_idx: usize,
    pub service_id: String,
    pub http: FetchResult<Option<HttpMetricsResult>>,
}

/// Single-service metrics app state
pub struct MetricsApp {
    // Identity
    pub service_name: String,
    pub environment_name: String,
    pub is_db: bool,
    pub db_stats_supported: bool,
    pub active_tab: ActiveTab,

    // Section visibility (toggled at runtime via 1-5)
    pub show_cpu: bool,
    pub show_memory: bool,
    pub show_network: bool,
    pub show_volume: bool,
    pub show_http: bool,

    // Network series toggles (toggled via e/i keys)
    pub show_egress: bool,
    pub show_ingress: bool,

    // HTTP status code toggles (toggled via 6-9 keys)
    pub show_2xx: bool,
    pub show_3xx: bool,
    pub show_4xx: bool,
    pub show_5xx: bool,

    // Response time percentile toggles (toggled via F1-F4 keys)
    pub show_p50: bool,
    pub show_p90: bool,
    pub show_p95: bool,
    pub show_p99: bool,

    // Time range
    pub time_range_idx: usize,
    pub time_range_changed: bool,

    // Resource data (with raw_values for charts)
    pub cpu: Option<MetricSummary>,
    pub cpu_limit: Option<MetricSummary>,
    pub memory: Option<MetricSummary>,
    pub memory_limit: Option<MetricSummary>,
    pub network_tx: Option<MetricSummary>,
    pub network_rx: Option<MetricSummary>,
    pub disk: Option<MetricSummary>,
    pub volumes: Vec<VolumeMetrics>,

    // HTTP data
    pub http: Option<HttpMetricsResult>,

    // Database stats (fetched via SSH for database services)
    pub db_stats: Option<DatabaseStats>,
    pub db_stats_error: Option<String>,
    pub db_stats_handle: Option<JoinHandle<anyhow::Result<DatabaseStats>>>,
    pub db_stats_scroll: u16,

    // State
    pub last_refresh: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
    pub show_help: bool,
    pub force_refresh: bool,
    pub refreshing: bool,
}

impl MetricsApp {
    pub fn new(params: &ServiceTuiParams) -> Self {
        let time_range_idx = TIME_RANGES
            .iter()
            .position(|&r| r == params.since_label)
            .unwrap_or(0);

        Self {
            service_name: params.service_name.clone(),
            environment_name: params.environment_name.clone(),
            is_db: params.is_db,
            db_stats_supported: params.db_stats_supported,
            active_tab: ActiveTab::Metrics,
            show_cpu: params.sections.cpu,
            show_memory: params.sections.memory,
            show_network: params.sections.network,
            show_volume: params.sections.volume,
            show_http: params.sections.http,
            show_egress: true,
            show_ingress: true,
            show_2xx: true,
            show_3xx: true,
            show_4xx: true,
            show_5xx: true,
            show_p50: true,
            show_p90: true,
            show_p95: true,
            show_p99: true,
            time_range_idx,
            time_range_changed: false,
            cpu: None,
            cpu_limit: None,
            memory: None,
            memory_limit: None,
            network_tx: None,
            network_rx: None,
            disk: None,
            volumes: params.volumes.clone(),
            http: None,
            db_stats: None,
            db_stats_error: params.db_stats_preflight_error.clone(),
            db_stats_handle: None,
            db_stats_scroll: 0,
            last_refresh: None,
            error_message: None,
            show_help: false,
            force_refresh: false,
            refreshing: false,
        }
    }

    pub fn time_range_label(&self) -> &'static str {
        TIME_RANGES[self.time_range_idx]
    }

    pub fn poll_interval_secs(&self) -> u64 {
        POLL_INTERVALS_SECS[self.time_range_idx]
    }

    pub fn mark_refreshing(&mut self) {
        self.refreshing = true;
    }

    pub fn apply_refresh_result(&mut self, result: ServiceRefreshResult) {
        if result.time_range_idx != self.time_range_idx {
            return;
        }
        let fetched_at = result.fetched_at;

        if let Some(result) = result.resource {
            match result {
                Ok(result) => {
                    self.cpu = find_metric(&result.metrics, "CPU_USAGE").cloned();
                    self.cpu_limit = find_metric(&result.metrics, "CPU_LIMIT").cloned();
                    self.memory = find_metric(&result.metrics, "MEMORY_USAGE_GB").cloned();
                    self.memory_limit = find_metric(&result.metrics, "MEMORY_LIMIT_GB").cloned();
                    self.network_tx = find_metric(&result.metrics, "NETWORK_TX_GB").cloned();
                    self.network_rx = find_metric(&result.metrics, "NETWORK_RX_GB").cloned();
                    self.disk = find_metric(&result.metrics, "DISK_USAGE_GB").cloned();
                    self.error_message = None;
                }
                Err(e) => {
                    self.error_message = Some(format!("Resource fetch failed: {e}"));
                }
            }
        }

        if let Some(result) = result.http {
            match result {
                Ok(result) => {
                    self.http = result;
                    self.error_message = None;
                }
                Err(e) => {
                    self.error_message = Some(format!("HTTP fetch failed: {e}"));
                }
            }
        }

        self.last_refresh = Some(fetched_at);
        self.refreshing = false;
    }

    pub fn maybe_start_db_stats_fetch(&mut self, params: &ServiceTuiParams) {
        if !self.db_stats_supported
            || self.active_tab != ActiveTab::Stats
            || self.db_stats.is_some()
            || self.db_stats_handle.is_some()
            || self.db_stats_error.is_some()
        {
            return;
        }

        if let (Some(instance_id), Some(db_type)) =
            (params.service_instance_id.clone(), params.db_type.clone())
        {
            self.db_stats_handle = Some(tokio::spawn(async move {
                db_stats::fetch_db_stats(&instance_id, &db_type).await
            }));
        }
    }

    /// Handle mouse click events (tab switching)
    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        if !self.db_stats_supported {
            return;
        }
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let col = mouse.column;
            let row = mouse.row;
            // Tab bar: header(1) + padding(1) + tab labels(1)
            // Layout: "   Metrics   Stats "
            //          ^2      ^11 ^12   ^19
            if row == 2 {
                if (2..11).contains(&col) {
                    self.active_tab = ActiveTab::Metrics;
                } else if (12..19).contains(&col) {
                    self.active_tab = ActiveTab::Stats;
                }
            }
        }
    }

    /// Check if the background db_stats fetch has completed.
    /// Call this from the event loop before each draw.
    pub fn poll_db_stats(&mut self, params: &ServiceTuiParams) -> bool {
        if let Some(ref handle) = self.db_stats_handle {
            if handle.is_finished() {
                let handle = self.db_stats_handle.take().unwrap();
                // Use try_join (non-blocking since we know it's finished)
                match handle.now_or_never() {
                    Some(Ok(Ok(stats))) => {
                        self.db_stats = Some(stats);
                        self.db_stats_error = None;
                    }
                    Some(Ok(Err(e))) => {
                        let msg = match params.db_type.as_ref() {
                            Some(dt) => db_stats::diagnose_db_stats_failure(&e, dt),
                            None => format!("{e:#}"),
                        };
                        self.db_stats_error = Some(msg);
                    }
                    Some(Err(_)) => {
                        self.db_stats_error = Some("task panicked".to_string());
                    }
                    None => {} // shouldn't happen since is_finished() was true
                }
                return true;
            }
        }
        false
    }

    /// Returns true if the user wants to quit
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.show_help {
            // Any key dismisses the help overlay
            self.show_help = false;
            return false;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Char('1') => {
                self.show_cpu = !self.show_cpu;
                self.force_refresh = true;
            }
            KeyCode::Char('2') => {
                self.show_memory = !self.show_memory;
                self.force_refresh = true;
            }
            KeyCode::Char('3') => {
                self.show_network = !self.show_network;
                self.force_refresh = true;
            }
            KeyCode::Char('4') => {
                self.show_volume = !self.show_volume;
                self.force_refresh = true;
            }
            KeyCode::Char('5') => {
                if !self.is_db {
                    self.show_http = !self.show_http;
                    self.force_refresh = true;
                }
            }
            // Tab switching for database services
            KeyCode::Tab | KeyCode::BackTab => {
                if self.db_stats_supported {
                    self.active_tab = match self.active_tab {
                        ActiveTab::Metrics => ActiveTab::Stats,
                        ActiveTab::Stats => ActiveTab::Metrics,
                    };
                }
            }
            // Scroll db stats with arrow keys / j/k (only on Stats tab)
            KeyCode::Down | KeyCode::Char('j') => {
                if self.active_tab == ActiveTab::Stats {
                    self.db_stats_scroll = self.db_stats_scroll.saturating_add(1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.active_tab == ActiveTab::Stats {
                    self.db_stats_scroll = self.db_stats_scroll.saturating_sub(1);
                }
            }
            // Network series toggles
            KeyCode::Char('e') => self.show_egress = !self.show_egress,
            KeyCode::Char('i') => self.show_ingress = !self.show_ingress,
            // HTTP status code toggles
            KeyCode::Char('6') => self.show_2xx = !self.show_2xx,
            KeyCode::Char('7') => self.show_3xx = !self.show_3xx,
            KeyCode::Char('8') => self.show_4xx = !self.show_4xx,
            KeyCode::Char('9') => self.show_5xx = !self.show_5xx,
            // Response time percentile toggles
            KeyCode::F(1) => self.show_p50 = !self.show_p50,
            KeyCode::F(2) => self.show_p90 = !self.show_p90,
            KeyCode::F(3) => self.show_p95 = !self.show_p95,
            KeyCode::F(4) => self.show_p99 = !self.show_p99,
            KeyCode::Char('t') => {
                self.time_range_idx = (self.time_range_idx + 1) % TIME_RANGES.len();
                self.time_range_changed = true;
            }
            KeyCode::Char('r') => {
                // Debounce: skip if last refresh was < 2s ago
                let should_refresh = self
                    .last_refresh
                    .map(|t| (Utc::now() - t).num_seconds() >= 2)
                    .unwrap_or(true);
                if should_refresh {
                    if self.db_stats_supported {
                        self.db_stats = None;
                        self.db_stats_error = None;
                    }
                    self.force_refresh = true;
                }
            }
            KeyCode::Char('?') => self.show_help = true,
            _ => {}
        }
        false
    }
}

pub async fn fetch_service_refresh(
    params: ServiceTuiParams,
    request_id: u64,
    time_range_idx: usize,
    options: ServiceRefreshOptions,
) -> ServiceRefreshResult {
    let now = Utc::now();
    let since_str = TIME_RANGES[time_range_idx];
    let start_date = match parse_time(since_str) {
        Ok(t) => t,
        Err(e) => {
            return ServiceRefreshResult {
                request_id,
                time_range_idx,
                resource: Some(Err(format!("Failed to parse time range: {e}"))),
                http: None,
                fetched_at: Utc::now(),
            };
        }
    };

    let duration = now - start_date;
    let sample_rate = compute_sample_rate(duration);

    let needs_resource =
        options.show_cpu || options.show_memory || options.show_network || options.show_volume;
    let measurements = if needs_resource {
        build_measurements(
            options.show_cpu,
            options.show_memory,
            options.show_network,
            options.show_volume,
        )
    } else {
        vec![]
    };
    let wants_http = options.show_http && !options.is_db;

    let resource_fut = async {
        if needs_resource {
            Some(
                fetch_resource_metrics(FetchResourceMetricsParams {
                    client: &params.client,
                    backboard: &params.backboard,
                    service_id: &params.service_id,
                    environment_id: &params.environment_id,
                    start_date,
                    end_date: None,
                    measurements,
                    sample_rate_seconds: Some(sample_rate),
                    include_raw: true,
                })
                .await
                .map_err(|e| format!("{e:#}")),
            )
        } else {
            None
        }
    };

    let http_fut = async {
        if wants_http {
            Some(
                fetch_http_metrics(FetchHttpMetricsParams {
                    client: &params.client,
                    backboard: &params.backboard,
                    service_id: &params.service_id,
                    environment_id: &params.environment_id,
                    start_date,
                    end_date: now,
                    step_seconds: Some(sample_rate),
                    method: params.method.clone(),
                    path: params.path.clone(),
                    include_time_series: true,
                })
                .await
                .map_err(|e| format!("{e:#}")),
            )
        } else {
            None
        }
    };

    let (resource, http) = tokio::join!(resource_fut, http_fut);

    ServiceRefreshResult {
        request_id,
        time_range_idx,
        resource,
        http,
        fetched_at: Utc::now(),
    }
}

/// Cached time-series data for one service's detail view
pub struct DetailCacheEntry {
    pub cpu: Option<MetricSummary>,
    pub cpu_limit: Option<MetricSummary>,
    pub memory: Option<MetricSummary>,
    pub memory_limit: Option<MetricSummary>,
    pub network_tx: Option<MetricSummary>,
    pub network_rx: Option<MetricSummary>,
    pub disk: Option<MetricSummary>,
    pub volumes: Vec<VolumeMetrics>,
    pub http: Option<HttpMetricsResult>,
    pub is_database: bool,
    pub fetched_at: DateTime<Utc>,
    pub time_range_idx: usize,
}

/// Project-wide metrics app state (--all mode)
pub struct ProjectApp {
    pub project_name: String,
    pub environment_name: String,

    // Time range
    pub time_range_idx: usize,
    pub time_range_changed: bool,

    // Per-service data
    pub services: Vec<ServiceMetricsSummary>,

    // Selection / scroll
    pub selected_idx: usize,
    pub table_scroll_offset: usize,

    // Detail panel cache (keyed by service_id)
    pub detail_cache: HashMap<String, DetailCacheEntry>,
    pub detail_loading: bool,
    pub detail_loading_service_id: Option<String>,

    // Section toggles (shared across detail views)
    pub show_cpu: bool,
    pub show_memory: bool,
    pub show_network: bool,
    pub show_volume: bool,
    pub show_http: bool,
    pub show_egress: bool,
    pub show_ingress: bool,
    pub show_2xx: bool,
    pub show_3xx: bool,
    pub show_4xx: bool,
    pub show_5xx: bool,
    pub show_p50: bool,
    pub show_p90: bool,
    pub show_p95: bool,
    pub show_p99: bool,

    // State
    pub last_refresh: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
    pub show_help: bool,
    pub force_refresh: bool,
    pub refreshing: bool,
}

impl ProjectApp {
    pub fn new(params: &ProjectTuiParams) -> Self {
        let time_range_idx = TIME_RANGES
            .iter()
            .position(|&r| r == params.since_label)
            .unwrap_or(0);

        Self {
            project_name: params.project.name.clone(),
            environment_name: params.environment_name.clone(),
            time_range_idx,
            time_range_changed: false,
            services: vec![],
            selected_idx: 0,
            table_scroll_offset: 0,
            detail_cache: HashMap::new(),
            detail_loading: false,
            detail_loading_service_id: None,
            show_cpu: params.sections.cpu,
            show_memory: params.sections.memory,
            show_network: params.sections.network,
            show_volume: params.sections.volume,
            show_http: params.sections.http,
            show_egress: true,
            show_ingress: true,
            show_2xx: true,
            show_3xx: true,
            show_4xx: true,
            show_5xx: true,
            show_p50: true,
            show_p90: true,
            show_p95: true,
            show_p99: true,
            last_refresh: None,
            error_message: None,
            show_help: false,
            force_refresh: false,
            refreshing: false,
        }
    }

    pub fn time_range_label(&self) -> &'static str {
        TIME_RANGES[self.time_range_idx]
    }

    pub fn poll_interval_secs(&self) -> u64 {
        POLL_INTERVALS_SECS[self.time_range_idx]
    }

    pub fn selected_service(&self) -> Option<&ServiceMetricsSummary> {
        self.services.get(self.selected_idx)
    }

    pub fn selected_detail(&self) -> Option<&DetailCacheEntry> {
        let svc = self.services.get(self.selected_idx)?;
        let entry = self.detail_cache.get(&svc.service_id)?;
        if entry.time_range_idx != self.time_range_idx {
            return None;
        }
        Some(entry)
    }

    pub fn needs_detail_fetch(&self) -> bool {
        let Some(svc) = self.services.get(self.selected_idx) else {
            return false;
        };
        self.selected_detail().is_none()
            && self.detail_loading_service_id.as_deref() != Some(svc.service_id.as_str())
    }

    pub fn selected_detail_request(&self, request_id: u64) -> Option<ProjectDetailRequest> {
        let svc = self.services.get(self.selected_idx)?;
        Some(ProjectDetailRequest {
            request_id,
            time_range_idx: self.time_range_idx,
            service_id: svc.service_id.clone(),
            is_database: svc.is_database,
            volumes: svc.volumes.clone(),
            options: ProjectRefreshOptions::from_app(self),
        })
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.services.is_empty() {
            return;
        }
        let max = self.services.len() - 1;
        self.selected_idx = if delta < 0 {
            self.selected_idx.saturating_sub(delta.unsigned_abs())
        } else {
            (self.selected_idx + delta as usize).min(max)
        };
    }

    pub fn clamp_selection(&mut self) {
        if self.services.is_empty() {
            self.selected_idx = 0;
        } else if self.selected_idx >= self.services.len() {
            self.selected_idx = self.services.len() - 1;
        }
    }

    /// Adjust scroll offset to keep the selected row visible.
    pub fn ensure_selection_visible(&mut self, visible_rows: usize) {
        if visible_rows == 0 || self.services.is_empty() {
            return;
        }
        if self.selected_idx < self.table_scroll_offset {
            self.table_scroll_offset = self.selected_idx;
        }
        if self.selected_idx >= self.table_scroll_offset + visible_rows {
            self.table_scroll_offset = self.selected_idx + 1 - visible_rows;
        }
    }

    pub fn invalidate_detail_cache(&mut self) {
        self.detail_cache.clear();
    }

    fn request_refresh_for_section_change(&mut self) {
        self.invalidate_detail_cache();
        self.force_refresh = true;
    }

    pub fn mark_refreshing(&mut self) {
        self.refreshing = true;
    }

    pub fn mark_detail_loading(&mut self, service_id: String) {
        self.detail_loading = true;
        self.detail_loading_service_id = Some(service_id);
    }

    pub fn apply_refresh_result(&mut self, result: ProjectRefreshResult) -> bool {
        if result.time_range_idx != self.time_range_idx {
            return false;
        }
        let fetched_at = result.fetched_at;
        let mut applied_services = false;

        match result.services {
            Ok(mut services) => {
                applied_services = true;
                let prev_selected_id = self.selected_service().map(|s| s.service_id.clone());
                let previous_http = self
                    .services
                    .iter()
                    .filter_map(|svc| svc.http.clone().map(|http| (svc.service_id.clone(), http)))
                    .collect::<HashMap<_, _>>();
                for svc in &mut services {
                    if svc.http.is_none() {
                        svc.http = previous_http.get(&svc.service_id).cloned();
                    }
                }

                self.services = services;
                self.error_message = None;

                // Detail panels contain time-series data, so refresh them whenever
                // the project summary refreshes instead of reusing stale charts.
                self.detail_cache.clear();

                if let Some(id) = prev_selected_id {
                    if let Some(idx) = self.services.iter().position(|s| s.service_id == id) {
                        self.selected_idx = idx;
                    }
                }
                self.clamp_selection();
            }
            Err(e) => {
                self.error_message = Some(format!("Fetch failed: {e}"));
            }
        }

        self.last_refresh = Some(fetched_at);
        self.refreshing = false;
        applied_services
    }

    pub fn apply_detail_result(&mut self, result: ProjectDetailResult) {
        let selected_id = self.selected_service().map(|s| s.service_id.as_str());
        if selected_id != Some(result.service_id.as_str()) {
            if self.detail_loading_service_id.as_deref() == Some(result.service_id.as_str()) {
                self.detail_loading = false;
                self.detail_loading_service_id = None;
            }
            return;
        }

        match result.entry {
            Ok(entry) if entry.time_range_idx == self.time_range_idx => {
                self.detail_cache.insert(result.service_id, entry);
                self.detail_loading = false;
                self.detail_loading_service_id = None;
            }
            Ok(_) => {}
            Err(e) => {
                self.error_message = Some(format!("Detail fetch failed: {e}"));
                self.detail_loading = false;
                self.detail_loading_service_id = None;
            }
        }
    }

    pub fn http_summary_jobs(&self) -> Vec<String> {
        if !self.show_http {
            return vec![];
        }
        self.services
            .iter()
            .filter(|svc| !svc.is_database)
            .map(|svc| svc.service_id.clone())
            .collect()
    }

    pub fn apply_http_result(&mut self, result: ProjectHttpResult) {
        if result.time_range_idx != self.time_range_idx {
            return;
        }
        let Some(service) = self
            .services
            .iter_mut()
            .find(|svc| svc.service_id == result.service_id)
        else {
            return;
        };

        if let Ok(http) = result.http {
            service.http = http;
        }
    }

    /// Returns true if the user wants to quit
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.show_help {
            self.show_help = false;
            return false;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,

            // Navigation
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Home => self.move_selection(-(self.services.len() as isize)),
            KeyCode::End => self.move_selection(self.services.len() as isize),

            // Section toggles (detail panel)
            KeyCode::Char('1') => {
                self.show_cpu = !self.show_cpu;
                self.request_refresh_for_section_change();
            }
            KeyCode::Char('2') => {
                self.show_memory = !self.show_memory;
                self.request_refresh_for_section_change();
            }
            KeyCode::Char('3') => {
                self.show_network = !self.show_network;
                self.request_refresh_for_section_change();
            }
            KeyCode::Char('4') => {
                self.show_volume = !self.show_volume;
                self.request_refresh_for_section_change();
            }
            KeyCode::Char('5') => {
                if self.selected_service().is_none_or(|s| !s.is_database) {
                    self.show_http = !self.show_http;
                    self.request_refresh_for_section_change();
                }
            }
            KeyCode::Char('e') => self.show_egress = !self.show_egress,
            KeyCode::Char('i') => self.show_ingress = !self.show_ingress,
            KeyCode::Char('6') => self.show_2xx = !self.show_2xx,
            KeyCode::Char('7') => self.show_3xx = !self.show_3xx,
            KeyCode::Char('8') => self.show_4xx = !self.show_4xx,
            KeyCode::Char('9') => self.show_5xx = !self.show_5xx,
            KeyCode::F(1) => self.show_p50 = !self.show_p50,
            KeyCode::F(2) => self.show_p90 = !self.show_p90,
            KeyCode::F(3) => self.show_p95 = !self.show_p95,
            KeyCode::F(4) => self.show_p99 = !self.show_p99,

            KeyCode::Char('t') => {
                self.time_range_idx = (self.time_range_idx + 1) % TIME_RANGES.len();
                self.time_range_changed = true;
                self.invalidate_detail_cache();
            }
            KeyCode::Char('r') => {
                let should_refresh = self
                    .last_refresh
                    .map(|t| (Utc::now() - t).num_seconds() >= 2)
                    .unwrap_or(true);
                if should_refresh {
                    self.force_refresh = true;
                }
            }
            KeyCode::Char('?') => self.show_help = true,
            _ => {}
        }
        false
    }
}

pub async fn fetch_project_refresh(
    params: ProjectTuiParams,
    request_id: u64,
    time_range_idx: usize,
    options: ProjectRefreshOptions,
) -> ProjectRefreshResult {
    let now = Utc::now();
    let since_str = TIME_RANGES[time_range_idx];
    let start_date = match parse_time(since_str) {
        Ok(t) => t,
        Err(e) => {
            return ProjectRefreshResult {
                request_id,
                time_range_idx,
                services: Err(format!("Failed to parse time range: {e}")),
                fetched_at: Utc::now(),
            };
        }
    };

    let duration = now - start_date;
    let sample_rate = compute_sample_rate(duration);
    let measurements = build_measurements(
        options.show_cpu,
        options.show_memory,
        options.show_network,
        false,
    );

    let services = match fetch_project_metrics(
        FetchProjectMetricsParams {
            client: &params.client,
            backboard: &params.backboard,
            project_id: &params.project_id,
            environment_id: &params.environment_id,
            start_date,
            end_date: None,
            measurements,
            sample_rate_seconds: Some(sample_rate),
        },
        &params.project,
    )
    .await
    {
        Ok(mut services) => {
            if options.show_volume {
                for svc in &mut services {
                    svc.volumes = get_volume_metrics(
                        &params.project,
                        &params.environment_id,
                        &svc.service_id,
                    );
                }
            }

            for svc in &mut services {
                let service_instance =
                    find_service_instance(&params.project, &params.environment_id, &svc.service_id);
                let source_image = service_instance
                    .and_then(|si| si.source.as_ref())
                    .and_then(|src| src.image.as_deref());
                svc.is_database = is_database_service(source_image);
            }

            Ok(services)
        }
        Err(e) => Err(format!("{e:#}")),
    };

    ProjectRefreshResult {
        request_id,
        time_range_idx,
        services,
        fetched_at: Utc::now(),
    }
}

pub async fn fetch_project_http_summary(
    params: ProjectTuiParams,
    request_id: u64,
    time_range_idx: usize,
    service_id: String,
) -> ProjectHttpResult {
    let now = Utc::now();
    let since_str = TIME_RANGES[time_range_idx];
    let start_date = match parse_time(since_str) {
        Ok(t) => t,
        Err(e) => {
            return ProjectHttpResult {
                request_id,
                time_range_idx,
                service_id,
                http: Err(format!("Failed to parse time range: {e}")),
            };
        }
    };

    let duration = now - start_date;
    let sample_rate = compute_sample_rate(duration);
    let http = fetch_http_metrics(FetchHttpMetricsParams {
        client: &params.client,
        backboard: &params.backboard,
        service_id: &service_id,
        environment_id: &params.environment_id,
        start_date,
        end_date: now,
        step_seconds: Some(sample_rate),
        method: params.method.clone(),
        path: params.path.clone(),
        include_time_series: false,
    })
    .await
    .map_err(|e| format!("{e:#}"));

    ProjectHttpResult {
        request_id,
        time_range_idx,
        service_id,
        http,
    }
}

pub async fn fetch_project_detail(
    params: ProjectTuiParams,
    request: ProjectDetailRequest,
) -> ProjectDetailResult {
    let now = Utc::now();
    let since_str = TIME_RANGES[request.time_range_idx];
    let start_date = match parse_time(since_str) {
        Ok(t) => t,
        Err(e) => {
            return ProjectDetailResult {
                request_id: request.request_id,
                service_id: request.service_id,
                entry: Err(format!("Failed to parse time range: {e}")),
            };
        }
    };

    let duration = now - start_date;
    let sample_rate = compute_sample_rate(duration);
    let measurements = build_measurements(
        request.options.show_cpu,
        request.options.show_memory,
        request.options.show_network,
        request.options.show_volume,
    );
    let wants_http = request.options.show_http && !request.is_database;

    let resource_fut = async {
        fetch_resource_metrics(FetchResourceMetricsParams {
            client: &params.client,
            backboard: &params.backboard,
            service_id: &request.service_id,
            environment_id: &params.environment_id,
            start_date,
            end_date: None,
            measurements,
            sample_rate_seconds: Some(sample_rate),
            include_raw: true,
        })
        .await
    };

    let http_fut = async {
        if wants_http {
            Some(
                fetch_http_metrics(FetchHttpMetricsParams {
                    client: &params.client,
                    backboard: &params.backboard,
                    service_id: &request.service_id,
                    environment_id: &params.environment_id,
                    start_date,
                    end_date: now,
                    step_seconds: Some(sample_rate),
                    method: params.method.clone(),
                    path: params.path.clone(),
                    include_time_series: true,
                })
                .await,
            )
        } else {
            None
        }
    };

    let (resource_result, http_result) = tokio::join!(resource_fut, http_fut);

    let mut entry = DetailCacheEntry {
        cpu: None,
        cpu_limit: None,
        memory: None,
        memory_limit: None,
        network_tx: None,
        network_rx: None,
        disk: None,
        volumes: request.volumes,
        http: None,
        is_database: request.is_database,
        fetched_at: Utc::now(),
        time_range_idx: request.time_range_idx,
    };

    let entry_result = match resource_result {
        Ok(result) => {
            entry.cpu = find_metric(&result.metrics, "CPU_USAGE").cloned();
            entry.cpu_limit = find_metric(&result.metrics, "CPU_LIMIT").cloned();
            entry.memory = find_metric(&result.metrics, "MEMORY_USAGE_GB").cloned();
            entry.memory_limit = find_metric(&result.metrics, "MEMORY_LIMIT_GB").cloned();
            entry.network_tx = find_metric(&result.metrics, "NETWORK_TX_GB").cloned();
            entry.network_rx = find_metric(&result.metrics, "NETWORK_RX_GB").cloned();
            entry.disk = find_metric(&result.metrics, "DISK_USAGE_GB").cloned();

            if let Some(http_result) = http_result {
                match http_result {
                    Ok(http) => entry.http = http,
                    Err(e) => {
                        return ProjectDetailResult {
                            request_id: request.request_id,
                            service_id: request.service_id,
                            entry: Err(format!("{e:#}")),
                        };
                    }
                }
            }

            Ok(entry)
        }
        Err(e) => Err(format!("{e:#}")),
    };

    ProjectDetailResult {
        request_id: request.request_id,
        service_id: request.service_id,
        entry: entry_result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_summary(service_id: &str) -> ServiceMetricsSummary {
        ServiceMetricsSummary {
            service_id: service_id.to_string(),
            service_name: "api".to_string(),
            cpu: None,
            cpu_limit: None,
            memory: None,
            memory_limit: None,
            network_tx: None,
            network_rx: None,
            http: None,
            volumes: vec![],
            is_database: false,
        }
    }

    fn detail_entry(time_range_idx: usize) -> DetailCacheEntry {
        DetailCacheEntry {
            cpu: None,
            cpu_limit: None,
            memory: None,
            memory_limit: None,
            network_tx: None,
            network_rx: None,
            disk: None,
            volumes: vec![],
            http: None,
            is_database: false,
            fetched_at: Utc::now(),
            time_range_idx,
        }
    }

    fn project_app_with_cached_detail() -> ProjectApp {
        let services = vec![service_summary("svc_1")];
        let detail_cache = HashMap::from([("svc_1".to_string(), detail_entry(0))]);

        ProjectApp {
            project_name: "project".to_string(),
            environment_name: "production".to_string(),
            time_range_idx: 0,
            time_range_changed: false,
            services,
            selected_idx: 0,
            table_scroll_offset: 0,
            detail_cache,
            detail_loading: false,
            detail_loading_service_id: None,
            show_cpu: true,
            show_memory: true,
            show_network: true,
            show_volume: true,
            show_http: true,
            show_egress: true,
            show_ingress: true,
            show_2xx: true,
            show_3xx: true,
            show_4xx: true,
            show_5xx: true,
            show_p50: true,
            show_p90: true,
            show_p95: true,
            show_p99: true,
            last_refresh: None,
            error_message: None,
            show_help: false,
            force_refresh: false,
            refreshing: true,
        }
    }

    #[test]
    fn project_refresh_invalidates_selected_detail_cache() {
        let mut app = project_app_with_cached_detail();

        let applied = app.apply_refresh_result(ProjectRefreshResult {
            request_id: 1,
            time_range_idx: 0,
            services: Ok(vec![service_summary("svc_1")]),
            fetched_at: Utc::now(),
        });

        assert!(applied);
        assert!(app.detail_cache.is_empty());
        assert!(app.needs_detail_fetch());
    }
}
