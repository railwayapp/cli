use std::collections::HashMap;

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use futures::FutureExt;

use crate::controllers::db_stats::{self, DatabaseStats};
use crate::controllers::metrics::{
    FetchHttpMetricsParams, FetchProjectMetricsParams, FetchResourceMetricsParams,
    HttpMetricsResult, MetricSummary, ServiceMetricsSummary, VolumeMetrics, compute_sample_rate,
    fetch_http_metrics, fetch_project_metrics, fetch_resource_metrics, find_metric,
    get_volume_metrics, is_database_service,
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
        }
    }

    pub fn time_range_label(&self) -> &'static str {
        TIME_RANGES[self.time_range_idx]
    }

    pub fn poll_interval_secs(&self) -> u64 {
        POLL_INTERVALS_SECS[self.time_range_idx]
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

    pub async fn refresh(&mut self, params: &ServiceTuiParams) {
        let now = Utc::now();
        let since_str = TIME_RANGES[self.time_range_idx];
        let start_date = match parse_time(since_str) {
            Ok(t) => t,
            Err(e) => {
                self.error_message = Some(format!("Failed to parse time range: {e}"));
                return;
            }
        };

        let duration = now - start_date;
        let sample_rate = compute_sample_rate(duration);

        let needs_resource =
            self.show_cpu || self.show_memory || self.show_network || self.show_volume;
        let measurements = if needs_resource {
            build_measurements(
                self.show_cpu,
                self.show_memory,
                self.show_network,
                self.show_volume,
            )
        } else {
            vec![]
        };

        let wants_http = self.show_http && !self.is_db;

        // Fetch resource and HTTP metrics in parallel
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
                    .await,
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
                    .await,
                )
            } else {
                None
            }
        };

        let (resource_result, http_result) = tokio::join!(resource_fut, http_fut);

        if let Some(result) = resource_result {
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

        if let Some(result) = http_result {
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

        self.last_refresh = Some(Utc::now());
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
    pub fn poll_db_stats(&mut self, params: &ServiceTuiParams) {
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
            }
        }
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
        !self.detail_loading && self.selected_detail().is_none()
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

    pub async fn refresh_selected_detail(&mut self, params: &ProjectTuiParams) {
        let svc = match self.services.get(self.selected_idx) {
            Some(s) => s,
            None => return,
        };
        let service_id = svc.service_id.clone();
        let is_database = svc.is_database;
        let volumes = svc.volumes.clone();

        self.detail_loading = true;

        let now = Utc::now();
        let since_str = TIME_RANGES[self.time_range_idx];
        let start_date = match parse_time(since_str) {
            Ok(t) => t,
            Err(_) => {
                self.detail_loading = false;
                return;
            }
        };
        let duration = now - start_date;
        let sample_rate = compute_sample_rate(duration);

        let measurements = build_measurements(
            self.show_cpu,
            self.show_memory,
            self.show_network,
            self.show_volume,
        );

        let wants_http = self.show_http && !is_database;

        let resource_fut = async {
            Some(
                fetch_resource_metrics(FetchResourceMetricsParams {
                    client: &params.client,
                    backboard: &params.backboard,
                    service_id: &service_id,
                    environment_id: &params.environment_id,
                    start_date,
                    end_date: None,
                    measurements,
                    sample_rate_seconds: Some(sample_rate),
                    include_raw: true,
                })
                .await,
            )
        };

        let http_fut = async {
            if wants_http {
                Some(
                    fetch_http_metrics(FetchHttpMetricsParams {
                        client: &params.client,
                        backboard: &params.backboard,
                        service_id: &service_id,
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
            volumes,
            http: None,
            is_database,
            fetched_at: Utc::now(),
            time_range_idx: self.time_range_idx,
        };

        if let Some(Ok(result)) = resource_result {
            entry.cpu = find_metric(&result.metrics, "CPU_USAGE").cloned();
            entry.cpu_limit = find_metric(&result.metrics, "CPU_LIMIT").cloned();
            entry.memory = find_metric(&result.metrics, "MEMORY_USAGE_GB").cloned();
            entry.memory_limit = find_metric(&result.metrics, "MEMORY_LIMIT_GB").cloned();
            entry.network_tx = find_metric(&result.metrics, "NETWORK_TX_GB").cloned();
            entry.network_rx = find_metric(&result.metrics, "NETWORK_RX_GB").cloned();
            entry.disk = find_metric(&result.metrics, "DISK_USAGE_GB").cloned();
        }

        if let Some(Ok(result)) = http_result {
            entry.http = result;
        }

        self.detail_cache.insert(service_id, entry);
        self.detail_loading = false;
    }

    pub async fn refresh(&mut self, params: &ProjectTuiParams) {
        let now = Utc::now();
        let since_str = TIME_RANGES[self.time_range_idx];
        let start_date = match parse_time(since_str) {
            Ok(t) => t,
            Err(e) => {
                self.error_message = Some(format!("Failed to parse time range: {e}"));
                return;
            }
        };

        let duration = now - start_date;
        let sample_rate = compute_sample_rate(duration);

        let measurements =
            build_measurements(self.show_cpu, self.show_memory, self.show_network, false);

        // Preserve selection by service_id across refreshes (list reorders by CPU)
        let prev_selected_id = self.selected_service().map(|s| s.service_id.clone());

        match fetch_project_metrics(
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
                if self.show_volume {
                    for svc in &mut services {
                        svc.volumes = get_volume_metrics(
                            &params.project,
                            &params.environment_id,
                            &svc.service_id,
                        );
                    }
                }

                if self.show_http {
                    let end = now;

                    for svc in &mut services {
                        let service_instance = find_service_instance(
                            &params.project,
                            &params.environment_id,
                            &svc.service_id,
                        );
                        let source_image = service_instance
                            .and_then(|si| si.source.as_ref())
                            .and_then(|src| src.image.as_deref());
                        svc.is_database = is_database_service(source_image);
                    }

                    let http_futures: Vec<_> = services
                        .iter()
                        .enumerate()
                        .filter(|(_, svc)| !svc.is_database)
                        .map(|(i, svc)| {
                            let params = FetchHttpMetricsParams {
                                client: &params.client,
                                backboard: &params.backboard,
                                service_id: &svc.service_id,
                                environment_id: &params.environment_id,
                                start_date,
                                end_date: end,
                                step_seconds: Some(sample_rate),
                                method: params.method.clone(),
                                path: params.path.clone(),
                                include_time_series: false,
                            };
                            async move { (i, fetch_http_metrics(params).await) }
                        })
                        .collect();

                    let results = futures::future::join_all(http_futures).await;
                    for (i, result) in results {
                        if let Ok(http) = result {
                            services[i].http = http;
                        }
                    }
                }

                self.services = services;
                self.error_message = None;

                // Evict cache entries for services no longer in the project
                self.detail_cache
                    .retain(|id, _| self.services.iter().any(|s| s.service_id == *id));

                // Restore selection by service_id
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

        self.last_refresh = Some(Utc::now());

        self.refresh_selected_detail(params).await;
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
