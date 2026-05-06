use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Dataset, Gauge, GraphType, Padding, Paragraph},
};

use crate::controllers::metrics::{
    MetricDataPoint, MetricSummary, format_count, format_cpu, format_gb, format_mb, pct,
    utilization,
};

use super::app::{ActiveTab, MetricsApp, ProjectApp};

// ─── Colors (matching Railway dashboard) ─────────────────────────────────────

/// CPU + Memory use a blue-purple in the dashboard; closest 256-color match
const CPU_COLOR: Color = Color::Indexed(63); // blue-purple (#5f5fff)
const MEMORY_COLOR: Color = Color::Indexed(63); // same blue-purple as dashboard
const EGRESS_COLOR: Color = Color::Yellow;
const INGRESS_COLOR: Color = Color::Blue;
const DISK_COLOR: Color = Color::Magenta;
// Response Time percentile colors (matching dashboard legend exactly)
const P50_COLOR: Color = Color::Blue; // p50 (median) — blue
const P90_COLOR: Color = Color::Yellow; // p90 — yellow/orange
const P95_COLOR: Color = Color::Magenta; // p95 — purple
const P99_COLOR: Color = Color::Red; // p99 — red
// HTTP status codes (matching dashboard stacked bar legend)
const STATUS_2XX: Color = Color::Blue; // blue in dashboard
const STATUS_3XX: Color = Color::Magenta; // purple in dashboard
const STATUS_4XX: Color = Color::Yellow; // yellow/gold in dashboard
const STATUS_5XX: Color = Color::Red; // dark red in dashboard
const ERROR_RATE_COLOR: Color = Color::Red; // error rate line — red/pink
const BORDER_COLOR: Color = Color::DarkGray;
const LABEL_COLOR: Color = Color::DarkGray;

fn health_color(pct: f64) -> Color {
    if pct >= 85.0 {
        Color::Red
    } else if pct >= 60.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn error_rate_color(rate: f64) -> Color {
    if rate >= 10.0 {
        Color::Red
    } else if rate >= 5.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

// ─── Data helpers ────────────────────────────────────────────────────────────

/// Convert points to chart coordinates using a shared timestamp origin.
fn to_chart_data_from(points: &[MetricDataPoint], origin_ts: i64) -> Vec<(f64, f64)> {
    let t0 = origin_ts as f64;
    points.iter().map(|p| (p.ts as f64 - t0, p.value)).collect()
}

/// Build shared X-axis bounds and labels for multiple series in one chart.
fn time_bounds_and_labels_for_series(
    series: &[&[MetricDataPoint]],
) -> (i64, f64, f64, Vec<String>) {
    let Some(start_ts) = series
        .iter()
        .filter_map(|points| points.first().map(|point| point.ts))
        .min()
    else {
        return (0, 0.0, 1.0, vec![]);
    };
    let end_ts = series
        .iter()
        .filter_map(|points| points.last().map(|point| point.ts))
        .max()
        .unwrap_or(start_ts);
    let (x_min, x_max, labels) = time_bounds_and_labels_from_range(start_ts, end_ts);
    (start_ts, x_min, x_max, labels)
}

fn time_bounds_and_labels_from_range(start_ts: i64, end_ts: i64) -> (f64, f64, Vec<String>) {
    let t0 = start_ts as f64;
    let t_end = end_ts as f64;
    let range = (t_end - t0).max(1.0);

    let num_labels = 4;
    let step = range / num_labels as f64;
    let labels: Vec<String> = (0..=num_labels)
        .map(|i| {
            let ts = t0 + step * i as f64;
            chrono::DateTime::from_timestamp(ts as i64, 0)
                .map(|dt| {
                    // Use local time with AM/PM format like the Railway dashboard
                    let local: chrono::DateTime<chrono::Local> = dt.into();
                    local.format("%-I:%M %p").to_string()
                })
                .unwrap_or_default()
        })
        .collect();

    (0.0, range, labels)
}

/// Create a consistent card block with inner padding
fn card_block(title: &str) -> Block<'_> {
    Block::default()
        .title(format!(" {title} "))
        .title_style(Style::default().add_modifier(Modifier::BOLD))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER_COLOR))
        .padding(Padding::new(2, 2, 1, 1)) // left, right, top, bottom
}

/// Uniform gap between all cards
const CARD_SPACING: u16 = 1;

/// Compute the left offset for text below a Chart to align with the chart's plot area.
/// Mirrors ratatui's `max_width_of_labels_left_of_y_axis` + 1 for the axis line.
fn chart_left_pad(y_labels: &[String], x_labels: &[String]) -> String {
    let max_y = y_labels.iter().map(|l| l.len()).max().unwrap_or(0);
    // First X-axis label extends left of the Y-axis (Alignment::Left: width - 1)
    let first_x = x_labels
        .first()
        .map(|l| l.len().saturating_sub(1))
        .unwrap_or(0);
    let max_w = max_y.max(first_x);
    // +1 for the axis line itself
    " ".repeat(max_w + 1)
}

/// Build a togglable legend entry: colored dot + label + key hint
fn make_legend_item<'a>(active: bool, color: Color, label: &str, key: &str) -> Vec<Span<'a>> {
    let dot_style = if active {
        Style::default().fg(color)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let text_style = if active {
        Style::default()
    } else {
        Style::default().fg(Color::DarkGray)
    };
    vec![
        Span::styled("●", dot_style),
        Span::styled(format!(" {label}"), text_style),
        Span::styled(
            format!(" [{key}]"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ]
}

// ─── Single-service rendering ────────────────────────────────────────────────

pub fn render_service(app: &MetricsApp, frame: &mut Frame) {
    let area = frame.area();

    if area.width < 60 || area.height < 10 {
        let msg = Paragraph::new("Terminal too small. Please resize (min 60x10).")
            .style(Style::default().fg(Color::Yellow));
        frame.render_widget(msg, area);
        return;
    }

    // For database services with native stats: header + tab bar + content + help bar
    // For non-database: header + content + help bar (no tab bar)
    if app.db_stats_supported {
        let outer = Layout::vertical([
            Constraint::Length(1), // header
            Constraint::Length(3), // tab bar (padding + tabs + padding)
            Constraint::Min(0),    // content
            Constraint::Length(1), // help bar
        ])
        .split(area);

        render_header(app, frame, outer[0]);
        render_tab_bar(app, frame, outer[1]);

        match app.active_tab {
            ActiveTab::Metrics => render_metrics_content(app, frame, outer[2]),
            ActiveTab::Stats => render_stats_content(app, frame, outer[2]),
        }

        render_service_help_bar(app, frame, outer[3]);
    } else {
        // Non-database: no tabs, original layout
        let outer = Layout::vertical([
            Constraint::Length(2), // header
            Constraint::Min(0),    // content
            Constraint::Length(1), // help bar
        ])
        .split(area);

        render_header(app, frame, outer[0]);
        render_metrics_content(app, frame, outer[1]);
        render_service_help_bar(app, frame, outer[2]);
    }

    if app.show_help {
        render_help_overlay(frame, area);
    }
}

fn render_tab_bar(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let active = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let inactive = Style::default().fg(Color::DarkGray);

    let line = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            " Metrics ",
            if app.active_tab == ActiveTab::Metrics {
                active
            } else {
                inactive
            },
        ),
        Span::raw(" "),
        Span::styled(
            " Stats ",
            if app.active_tab == ActiveTab::Stats {
                active
            } else {
                inactive
            },
        ),
    ]);

    // Center the tabs vertically in the 3-row area
    let inner = Layout::vertical([
        Constraint::Length(1), // top padding
        Constraint::Length(1), // tab labels
        Constraint::Length(1), // bottom padding
    ])
    .split(area);

    frame.render_widget(Paragraph::new(line), inner[1]);
}

fn render_metrics_content(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let show_cpu_mem = app.show_cpu || app.show_memory;
    let show_net_req = app.show_network || (app.show_http && !app.is_db);
    let show_err_resp = app.show_http && !app.is_db;
    let show_vol = app.show_volume && (!app.volumes.is_empty() || app.disk.is_some());
    let net_vol_combined = show_net_req && show_vol;

    let mut constraints: Vec<Constraint> = Vec::new();
    let mut chart_rows = 0u32;
    if show_cpu_mem {
        chart_rows += 1;
    }
    if net_vol_combined {
        chart_rows += 1;
    } else if show_net_req {
        chart_rows += 1;
    }
    if show_err_resp {
        chart_rows += 1;
    }
    let chart_rows = chart_rows.max(1);

    if show_cpu_mem {
        constraints.push(Constraint::Ratio(1, chart_rows));
    }
    if net_vol_combined {
        constraints.push(Constraint::Ratio(1, chart_rows));
    } else {
        if show_net_req {
            constraints.push(Constraint::Ratio(1, chart_rows));
        }
    }
    if show_err_resp {
        constraints.push(Constraint::Ratio(1, chart_rows));
    }
    if !net_vol_combined {
        if show_vol {
            constraints.push(Constraint::Length(6));
        }
    }
    constraints.push(Constraint::Min(0)); // filler

    let chunks = Layout::vertical(constraints)
        .spacing(CARD_SPACING)
        .split(area);

    let mut idx = 0;
    if show_cpu_mem {
        render_cpu_memory(app, frame, chunks[idx]);
        idx += 1;
    }
    if net_vol_combined {
        let cols = Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)])
            .spacing(CARD_SPACING)
            .split(chunks[idx]);
        render_network_http(app, frame, cols[0]);
        render_volume(app, frame, cols[1]);
        idx += 1;
    } else {
        if show_net_req {
            render_network_http(app, frame, chunks[idx]);
            idx += 1;
        }
    }
    if show_err_resp {
        let row3_cols =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .spacing(CARD_SPACING)
                .split(chunks[idx]);
        render_error_rate_chart(app, frame, row3_cols[0]);
        render_http_latency(app, frame, row3_cols[1]);
        idx += 1;
    }
    if !net_vol_combined && show_vol {
        render_volume(app, frame, chunks[idx]);
        idx += 1;
    }

    if let Some(ref err) = app.error_message {
        let filler_idx = chunks.len() - 1;
        if idx <= filler_idx {
            let err_line = Paragraph::new(format!(" {err}")).style(Style::default().fg(Color::Red));
            frame.render_widget(err_line, chunks[filler_idx]);
        }
    }
}

fn render_stats_content(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    if app.db_stats.is_some() {
        render_db_stats(app, frame, area);
    } else {
        let msg = if let Some(ref e) = app.db_stats_error {
            let mut lines = String::from("  Database stats unavailable:\n");
            for line in e.lines() {
                lines.push_str("    ");
                lines.push_str(line);
                lines.push('\n');
            }
            lines
        } else {
            "  Loading database stats...".to_string()
        };
        let style = if app.db_stats_error.is_some() {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        frame.render_widget(Paragraph::new(msg).style(style), area);
    }
}

fn render_header(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let refresh_str = app
        .last_refresh
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "loading...".to_string());

    let header = Line::from(vec![
        Span::styled(
            format!("  {}", app.service_name),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::default().fg(LABEL_COLOR)),
        Span::raw(format!("env {}", app.environment_name)),
        Span::styled("  ·  ", Style::default().fg(LABEL_COLOR)),
        Span::raw(format!("last {}", app.time_range_label())),
        Span::styled("  ·  ", Style::default().fg(LABEL_COLOR)),
        Span::styled(
            format!("refreshed {refresh_str}"),
            Style::default().fg(LABEL_COLOR),
        ),
        if app.refreshing {
            Span::styled("  ·  refreshing", Style::default().fg(Color::Yellow))
        } else {
            Span::raw("")
        },
    ]);

    frame.render_widget(Paragraph::new(vec![header, Line::from("")]), area);
}

fn render_cpu_memory(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .spacing(CARD_SPACING)
        .split(area);

    if app.show_cpu {
        render_metric_chart(
            frame,
            cols[0],
            "CPU",
            CPU_COLOR,
            app.cpu.as_ref(),
            app.cpu_limit.as_ref(),
            format_cpu,
            "vCPU",
        );
    }

    if app.show_memory {
        let col = if app.show_cpu { cols[1] } else { cols[0] };
        render_metric_chart(
            frame,
            col,
            "Memory",
            MEMORY_COLOR,
            app.memory.as_ref(),
            app.memory_limit.as_ref(),
            format_gb,
            "GB",
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_metric_chart(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    color: Color,
    metric: Option<&MetricSummary>,
    limit: Option<&MetricSummary>,
    format_fn: fn(f64) -> String,
    _unit: &str,
) {
    let block = card_block(title);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    match metric {
        Some(m) if !m.raw_values.is_empty() => {
            let parts = Layout::vertical([
                Constraint::Min(3),
                Constraint::Length(1), // gap
                Constraint::Length(1), // stats
            ])
            .split(inner);

            let chart_data = &m.chart_data.points;
            let x_min = m.chart_data.x_min;
            let x_max = m.chart_data.x_max;
            let x_labels = &m.chart_data.labels;

            let y_max = m.max * 1.15;
            let y_max = if y_max < 0.001 { 1.0 } else { y_max };

            let dataset = Dataset::default()
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(color))
                .data(chart_data);

            let mut datasets = vec![dataset];

            let limit_data;
            if let Some(lim) = limit.filter(|l| l.current > 0.0) {
                limit_data = vec![(x_min, lim.current), (x_max, lim.current)];
                datasets.push(
                    Dataset::default()
                        .marker(symbols::Marker::Braille)
                        .graph_type(GraphType::Line)
                        .style(Style::default().fg(LABEL_COLOR))
                        .data(&limit_data),
                );
            }

            let x_axis = Axis::default()
                .style(Style::default().fg(LABEL_COLOR))
                .bounds([x_min, x_max])
                .labels(x_labels.to_vec());

            let y_label_strs = vec!["0".to_string(), format_fn(y_max / 2.0), format_fn(y_max)];
            let pad = chart_left_pad(&y_label_strs, x_labels);

            let y_axis = Axis::default()
                .style(Style::default().fg(LABEL_COLOR))
                .bounds([0.0, y_max])
                .labels(
                    y_label_strs
                        .iter()
                        .map(|s| Span::raw(s.clone()))
                        .collect::<Vec<_>>(),
                );

            let chart = Chart::new(datasets).x_axis(x_axis).y_axis(y_axis);
            frame.render_widget(chart, parts[0]);

            let limit_str = limit
                .filter(|l| l.current > 0.0)
                .map(|l| format!(" / {}", format_fn(l.current)))
                .unwrap_or_default();
            let util_str = utilization(
                m.current,
                limit.filter(|l| l.current > 0.0).map(|l| l.current),
            )
            .map(|p| {
                Span::styled(
                    format!(" ({:.0}%)", p),
                    Style::default().fg(health_color(p)),
                )
            });

            let mut spans = vec![
                Span::raw(pad),
                Span::styled("Now: ", Style::default().fg(LABEL_COLOR)),
                Span::styled(format_fn(m.current), Style::default().fg(color)),
                Span::raw(limit_str),
            ];
            if let Some(u) = util_str {
                spans.push(u);
            }
            spans.extend([
                Span::styled("  Avg: ", Style::default().fg(LABEL_COLOR)),
                Span::raw(format_fn(m.average)),
                Span::styled("  Max: ", Style::default().fg(LABEL_COLOR)),
                Span::raw(format_fn(m.max)),
            ]);

            frame.render_widget(Paragraph::new(Line::from(spans)), parts[2]);
        }
        _ => {
            let msg =
                Paragraph::new(format!(" No {title} data")).style(Style::default().fg(LABEL_COLOR));
            frame.render_widget(msg, inner);
        }
    }
}

fn render_network_http(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let show_net = app.show_network;
    let show_http = app.show_http && !app.is_db;

    let cols = if show_net && show_http {
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .spacing(CARD_SPACING)
            .split(area)
    } else {
        Layout::horizontal([Constraint::Percentage(100)]).split(area)
    };

    if show_net {
        render_network_chart(app, frame, cols[0]);
    }

    if show_http {
        let col = if show_net { cols[1] } else { cols[0] };
        render_http_requests(app, frame, col);
    }
}

fn render_network_chart(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let block = card_block(if app.is_db {
        "Public Network Traffic — use private networking for service→db"
    } else {
        "Public Network Traffic"
    });

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let has_tx = app
        .network_tx
        .as_ref()
        .is_some_and(|t| !t.raw_values.is_empty());
    let has_rx = app
        .network_rx
        .as_ref()
        .is_some_and(|r| !r.raw_values.is_empty());

    if !has_tx && !has_rx {
        let msg = Paragraph::new(" No network data").style(Style::default().fg(LABEL_COLOR));
        frame.render_widget(msg, inner);
        return;
    }

    let parts = Layout::vertical([
        Constraint::Min(3),    // chart
        Constraint::Length(1), // gap
        Constraint::Length(1), // legend
    ])
    .split(inner);

    let tx_points = app
        .network_tx
        .as_ref()
        .map(|m| m.raw_values.as_slice())
        .unwrap_or(&[]);
    let rx_points = app
        .network_rx
        .as_ref()
        .map(|m| m.raw_values.as_slice())
        .unwrap_or(&[]);

    let (origin_ts, x_min, x_max, x_labels) =
        time_bounds_and_labels_for_series(&[tx_points, rx_points]);

    let tx_data = app
        .network_tx
        .as_ref()
        .filter(|_| app.show_egress)
        .map(|t| to_chart_data_from(&t.raw_values, origin_ts))
        .unwrap_or_default();
    let rx_data = app
        .network_rx
        .as_ref()
        .filter(|_| app.show_ingress)
        .map(|r| to_chart_data_from(&r.raw_values, origin_ts))
        .unwrap_or_default();

    let mut y_max = 0.0f64;
    for (_, v) in tx_data.iter().chain(rx_data.iter()) {
        if *v > y_max {
            y_max = *v;
        }
    }
    y_max = (y_max * 1.15).max(0.001);

    let mut datasets = Vec::new();
    if !tx_data.is_empty() {
        datasets.push(
            Dataset::default()
                .name("Egress")
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(EGRESS_COLOR))
                .data(&tx_data),
        );
    }
    if !rx_data.is_empty() {
        datasets.push(
            Dataset::default()
                .name("Ingress")
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(INGRESS_COLOR))
                .data(&rx_data),
        );
    }

    let x_axis = Axis::default()
        .style(Style::default().fg(LABEL_COLOR))
        .bounds([x_min, x_max])
        .labels(x_labels.clone());

    let y_label_strs = vec!["0".to_string(), format_gb(y_max / 2.0), format_gb(y_max)];
    let pad = chart_left_pad(&y_label_strs, &x_labels);

    let y_axis = Axis::default()
        .style(Style::default().fg(LABEL_COLOR))
        .bounds([0.0, y_max])
        .labels(
            y_label_strs
                .iter()
                .map(|s| Span::raw(s.clone()))
                .collect::<Vec<_>>(),
        );

    let chart = Chart::new(datasets)
        .x_axis(x_axis)
        .y_axis(y_axis)
        .legend_position(None);
    frame.render_widget(chart, parts[0]);

    let mut spans = vec![Span::raw(pad)];
    spans.extend(make_legend_item(
        app.show_egress,
        EGRESS_COLOR,
        "Egress",
        "e",
    ));
    spans.extend(make_legend_item(
        app.show_ingress,
        INGRESS_COLOR,
        "Ingress",
        "i",
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), parts[2]);
}

fn render_http_requests(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let http = app.http.as_ref();

    let title = match http {
        Some(h) => format!("Requests ({} total)", format_count(h.total)),
        None => "Requests".to_string(),
    };

    let block = card_block(&title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let http = match http {
        Some(h) => h,
        None => {
            let msg = Paragraph::new(" No HTTP data").style(Style::default().fg(LABEL_COLOR));
            frame.render_widget(msg, inner);
            return;
        }
    };

    let parts = Layout::vertical([
        Constraint::Min(3),    // chart area
        Constraint::Length(1), // gap
        Constraint::Length(1), // status counts
        Constraint::Length(1), // gap
        Constraint::Length(1), // interactive legend
    ])
    .split(inner);

    let mut pad = String::new();

    let has_ts = http.time_series.is_some();

    if has_ts {
        let ts = http.time_series.as_ref().unwrap();
        let (origin_ts, x_min, x_max, x_labels) = time_bounds_and_labels_for_series(&[
            &ts.status_2xx_ts,
            &ts.status_3xx_ts,
            &ts.status_4xx_ts,
            &ts.status_5xx_ts,
        ]);
        let data_2xx = if app.show_2xx {
            to_chart_data_from(&ts.status_2xx_ts, origin_ts)
        } else {
            vec![]
        };
        let data_3xx = if app.show_3xx {
            to_chart_data_from(&ts.status_3xx_ts, origin_ts)
        } else {
            vec![]
        };
        let data_4xx = if app.show_4xx {
            to_chart_data_from(&ts.status_4xx_ts, origin_ts)
        } else {
            vec![]
        };
        let data_5xx = if app.show_5xx {
            to_chart_data_from(&ts.status_5xx_ts, origin_ts)
        } else {
            vec![]
        };

        let mut y_max = 0.0f64;
        for (_, v) in data_2xx
            .iter()
            .chain(data_3xx.iter())
            .chain(data_4xx.iter())
            .chain(data_5xx.iter())
        {
            if *v > y_max {
                y_max = *v;
            }
        }
        y_max = (y_max * 1.15).max(1.0);

        let mut datasets = vec![];
        if !data_2xx.is_empty() {
            datasets.push(
                Dataset::default()
                    .name("2xx")
                    .marker(symbols::Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::default().fg(STATUS_2XX))
                    .data(&data_2xx),
            );
        }
        if !data_3xx.is_empty() {
            datasets.push(
                Dataset::default()
                    .name("3xx")
                    .marker(symbols::Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::default().fg(STATUS_3XX))
                    .data(&data_3xx),
            );
        }
        if !data_4xx.is_empty() {
            datasets.push(
                Dataset::default()
                    .name("4xx")
                    .marker(symbols::Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::default().fg(STATUS_4XX))
                    .data(&data_4xx),
            );
        }
        if !data_5xx.is_empty() {
            datasets.push(
                Dataset::default()
                    .name("5xx")
                    .marker(symbols::Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::default().fg(STATUS_5XX))
                    .data(&data_5xx),
            );
        }

        let x_axis = Axis::default()
            .style(Style::default().fg(LABEL_COLOR))
            .bounds([x_min, x_max])
            .labels(x_labels.clone());

        let y_label_strs = vec![
            "0".to_string(),
            format_count(y_max as usize / 2),
            format_count(y_max as usize),
        ];
        pad = chart_left_pad(&y_label_strs, &x_labels);

        let y_axis = Axis::default()
            .style(Style::default().fg(LABEL_COLOR))
            .bounds([0.0, y_max])
            .labels(
                y_label_strs
                    .iter()
                    .map(|s| Span::raw(s.clone()))
                    .collect::<Vec<_>>(),
            );

        let chart = Chart::new(datasets).x_axis(x_axis).y_axis(y_axis);
        frame.render_widget(chart, parts[0]);
    } else {
        let msg = Line::from(vec![
            Span::raw(format!(" {} total", format_count(http.total))),
            Span::styled("  ·  ", Style::default().fg(LABEL_COLOR)),
            Span::raw("Err: "),
            Span::styled(
                format!("{:.1}%", http.error_rate),
                Style::default().fg(error_rate_color(http.error_rate)),
            ),
        ]);
        frame.render_widget(Paragraph::new(msg), parts[0]);
    }

    let total = http.total;
    let status_line = Line::from(vec![
        Span::raw(pad.clone()),
        Span::styled(
            format!(
                "2xx: {} ({:.1}%)",
                format_count(http.status_counts[2]),
                pct(http.status_counts[2], total)
            ),
            Style::default().fg(STATUS_2XX),
        ),
        Span::raw("   "),
        Span::styled(
            format!(
                "3xx: {} ({:.1}%)",
                format_count(http.status_counts[3]),
                pct(http.status_counts[3], total)
            ),
            Style::default().fg(STATUS_3XX),
        ),
        Span::raw("   "),
        Span::styled(
            format!(
                "4xx: {} ({:.1}%)",
                format_count(http.status_counts[4]),
                pct(http.status_counts[4], total)
            ),
            Style::default().fg(STATUS_4XX),
        ),
        Span::raw("   "),
        Span::styled(
            format!(
                "5xx: {} ({:.1}%)",
                format_count(http.status_counts[5]),
                pct(http.status_counts[5], total)
            ),
            Style::default().fg(STATUS_5XX),
        ),
    ]);
    frame.render_widget(Paragraph::new(status_line), parts[2]);

    let mut spans = vec![Span::raw(pad)];
    spans.extend(make_legend_item(app.show_2xx, STATUS_2XX, "2xx", "6"));
    spans.extend(make_legend_item(app.show_3xx, STATUS_3XX, "3xx", "7"));
    spans.extend(make_legend_item(app.show_4xx, STATUS_4XX, "4xx", "8"));
    spans.extend(make_legend_item(app.show_5xx, STATUS_5XX, "5xx", "9"));

    frame.render_widget(Paragraph::new(Line::from(spans)), parts[4]);
}

fn render_error_rate_chart(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let block = card_block("Request Error Rate");

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let http = match app.http.as_ref() {
        Some(h) if h.time_series.is_some() => h,
        Some(h) => {
            // No time-series, show summary text
            let msg = Line::from(vec![
                Span::raw(" Error Rate: "),
                Span::styled(
                    format!("{:.1}%", h.error_rate),
                    Style::default().fg(error_rate_color(h.error_rate)),
                ),
            ]);
            frame.render_widget(Paragraph::new(msg), inner);
            return;
        }
        None => {
            let msg = Paragraph::new(" No error data").style(Style::default().fg(LABEL_COLOR));
            frame.render_widget(msg, inner);
            return;
        }
    };

    let ts = http.time_series.as_ref().unwrap();
    let err_pct_data: Vec<(f64, f64)> = ts
        .error_rate_ts
        .iter()
        .zip(ts.request_rate_ts.iter())
        .map(|(err, total)| {
            let pct = if total.value > 0.0 {
                (err.value / total.value) * 100.0
            } else {
                0.0
            };
            (err.ts as f64 - ts.error_rate_ts[0].ts as f64, pct)
        })
        .collect();

    let (chart_data, x_min, x_max, x_labels): (&[(f64, f64)], f64, f64, Vec<String>) =
        if !err_pct_data.is_empty() {
            (
                &err_pct_data,
                ts.error_rate_chart.x_min,
                ts.error_rate_chart.x_max,
                ts.error_rate_chart.labels.clone(),
            )
        } else {
            (
                &ts.error_rate_chart.points,
                ts.error_rate_chart.x_min,
                ts.error_rate_chart.x_max,
                ts.error_rate_chart.labels.clone(),
            )
        };

    let mut y_max = 0.0f64;
    for (_, v) in chart_data.iter() {
        if *v > y_max {
            y_max = *v;
        }
    }
    y_max = (y_max * 1.15).max(1.0);

    let datasets = vec![
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(ERROR_RATE_COLOR))
            .data(chart_data),
    ];

    let x_axis = Axis::default()
        .style(Style::default().fg(LABEL_COLOR))
        .bounds([x_min, x_max])
        .labels(x_labels.clone());

    let y_axis = Axis::default()
        .style(Style::default().fg(LABEL_COLOR))
        .bounds([0.0, y_max])
        .labels(vec![
            Span::raw("0.0%"),
            Span::raw(format!("{:.1}%", y_max / 2.0)),
            Span::raw(format!("{:.1}%", y_max)),
        ]);

    let chart = Chart::new(datasets).x_axis(x_axis).y_axis(y_axis);
    frame.render_widget(chart, inner);
}

fn render_http_latency(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let block = card_block("Response Time");

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let http = match app.http.as_ref() {
        Some(h) if h.time_series.is_some() => h,
        _ => {
            let msg = Paragraph::new(" No latency data").style(Style::default().fg(LABEL_COLOR));
            frame.render_widget(msg, inner);
            return;
        }
    };

    let ts = http.time_series.as_ref().unwrap();

    let parts = Layout::vertical([
        Constraint::Min(3),    // chart
        Constraint::Length(1), // gap
        Constraint::Length(1), // legend
    ])
    .split(inner);

    let x_min = ts.p50_chart.x_min;
    let x_max = ts.p50_chart.x_max;
    let x_labels = ts.p50_chart.labels.clone();
    let p50_data: &[(f64, f64)] = if app.show_p50 {
        &ts.p50_chart.points
    } else {
        &[]
    };
    let p90_data: &[(f64, f64)] = if app.show_p90 {
        &ts.p90_chart.points
    } else {
        &[]
    };
    let p95_data: &[(f64, f64)] = if app.show_p95 {
        &ts.p95_chart.points
    } else {
        &[]
    };
    let p99_data: &[(f64, f64)] = if app.show_p99 {
        &ts.p99_chart.points
    } else {
        &[]
    };

    let mut y_max = 0.0f64;
    for (_, v) in p50_data
        .iter()
        .chain(p90_data.iter())
        .chain(p95_data.iter())
        .chain(p99_data.iter())
    {
        if *v > y_max {
            y_max = *v;
        }
    }
    y_max = (y_max * 1.15).max(1.0);

    let format_duration = |ms: f64| -> String {
        if ms >= 1000.0 {
            format!("{:.0} sec", ms / 1000.0)
        } else {
            format!("{:.0} ms", ms)
        }
    };

    let mut datasets = vec![];
    if !p50_data.is_empty() {
        datasets.push(
            Dataset::default()
                .name("p50")
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(P50_COLOR))
                .data(p50_data),
        );
    }
    if !p90_data.is_empty() {
        datasets.push(
            Dataset::default()
                .name("p90")
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(P90_COLOR))
                .data(p90_data),
        );
    }
    if !p95_data.is_empty() {
        datasets.push(
            Dataset::default()
                .name("p95")
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(P95_COLOR))
                .data(p95_data),
        );
    }
    if !p99_data.is_empty() {
        datasets.push(
            Dataset::default()
                .name("p99")
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(P99_COLOR))
                .data(p99_data),
        );
    }

    let x_axis = Axis::default()
        .style(Style::default().fg(LABEL_COLOR))
        .bounds([x_min, x_max])
        .labels(x_labels.clone());

    let y_label_strs = vec![
        format_duration(0.0),
        format_duration(y_max / 2.0),
        format_duration(y_max),
    ];
    let pad = chart_left_pad(&y_label_strs, &x_labels);

    let y_axis = Axis::default()
        .style(Style::default().fg(LABEL_COLOR))
        .bounds([0.0, y_max])
        .labels(
            y_label_strs
                .iter()
                .map(|s| Span::raw(s.clone()))
                .collect::<Vec<_>>(),
        );

    let chart = Chart::new(datasets).x_axis(x_axis).y_axis(y_axis);
    frame.render_widget(chart, parts[0]);

    let mut spans = vec![Span::raw(pad)];
    spans.extend(make_legend_item(app.show_p50, P50_COLOR, "p50", "F1"));
    spans.extend(make_legend_item(app.show_p90, P90_COLOR, "p90", "F2"));
    spans.extend(make_legend_item(app.show_p95, P95_COLOR, "p95", "F3"));
    spans.extend(make_legend_item(app.show_p99, P99_COLOR, "p99", "F4"));

    frame.render_widget(Paragraph::new(Line::from(spans)), parts[2]);
}

fn render_volume(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let vol_name = app
        .volumes
        .first()
        .map(|v| format!("Volume: {}", v.mount_path))
        .unwrap_or_else(|| "Disk".to_string());

    let block = card_block(&vol_name);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (current, limit_mb) = if let Some(vol) = app.volumes.first() {
        (vol.current_size_mb, vol.limit_size_mb)
    } else if let Some(ref disk) = app.disk {
        (disk.current * 1024.0, 0.0) // disk is in GB, convert to MB
    } else {
        let msg = Paragraph::new(" No volume data").style(Style::default().fg(LABEL_COLOR));
        frame.render_widget(msg, inner);
        return;
    };

    let ratio = if limit_mb > 0.0 {
        (current / limit_mb).min(1.0)
    } else {
        0.0
    };

    let pct_val = ratio * 100.0;
    let label = if limit_mb > 0.0 {
        format!(
            "{} / {} ({:.0}%)",
            format_mb(current),
            format_mb(limit_mb),
            pct_val
        )
    } else {
        format_mb(current)
    };

    let gauge_color = if limit_mb > 0.0 {
        health_color(pct_val)
    } else {
        DISK_COLOR
    };

    let parts = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);

    if limit_mb > 0.0 {
        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(gauge_color))
            .ratio(ratio)
            .label(label);
        frame.render_widget(gauge, parts[0]);
    } else {
        let text = Paragraph::new(format!(" Usage: {label}"));
        frame.render_widget(text, parts[0]);
    }

    if let Some(ref disk) = app.disk {
        let stats = Line::from(vec![
            Span::styled(" Avg: ", Style::default().fg(LABEL_COLOR)),
            Span::raw(format_gb(disk.average)),
            Span::styled("  Max: ", Style::default().fg(LABEL_COLOR)),
            Span::raw(format_gb(disk.max)),
        ]);
        frame.render_widget(Paragraph::new(stats), parts[1]);
    }
}

fn render_db_stats(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    use crate::controllers::db_stats::types::*;

    let stats = match &app.db_stats {
        Some(s) => s,
        None => return,
    };

    let title = match stats {
        DatabaseStats::PostgreSQL(_) => "Database Stats (PostgreSQL)",
        DatabaseStats::Redis(_) => "Database Stats (Redis)",
        DatabaseStats::MySQL(_) => "Database Stats (MySQL)",
        DatabaseStats::MongoDB(_) => "Database Stats (MongoDB)",
    };

    let block = card_block(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let l = Style::default().fg(LABEL_COLOR);
    let v = Style::default().fg(Color::White);
    let a = Style::default().fg(Color::Cyan);
    let hint_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC);

    fn hs(value: f64, warn: f64, crit: f64, inverted: bool) -> Style {
        if inverted {
            if value < crit {
                Style::default().fg(Color::Red)
            } else if value < warn {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Green)
            }
        } else if value > crit {
            Style::default().fg(Color::Red)
        } else if value > warn {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Green)
        }
    }

    fn fb(bytes: i64) -> String {
        if bytes >= 1_073_741_824 {
            format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
        } else if bytes >= 1_048_576 {
            format!("{:.1} MB", bytes as f64 / 1_048_576.0)
        } else if bytes >= 1024 {
            format!("{:.1} KB", bytes as f64 / 1024.0)
        } else {
            format!("{} B", bytes)
        }
    }

    fn fk(n: i64) -> String {
        if n >= 1_000_000 {
            format!("{:.1}M", n as f64 / 1_000_000.0)
        } else if n >= 1000 {
            format!("{:.1}K", n as f64 / 1000.0)
        } else {
            format!("{n}")
        }
    }

    let mut lines: Vec<Line> = Vec::new();

    match stats {
        DatabaseStats::PostgreSQL(pg) => {
            let util = if pg.connections.max_connections > 0 {
                pg.connections.total as f64 / pg.connections.max_connections as f64 * 100.0
            } else {
                0.0
            };
            lines.push(Line::from(vec![
                Span::styled("Connections  ", l),
                Span::styled(format!("{}", pg.connections.active), a),
                Span::styled(" active  ", l),
                Span::styled(format!("{}", pg.connections.idle), v),
                Span::styled(" idle  ", l),
                Span::styled(format!("{}", pg.connections.idle_in_transaction), v),
                Span::styled(" idle in txn  ", l),
                Span::styled(
                    format!(
                        "{} / {}",
                        pg.connections.total, pg.connections.max_connections
                    ),
                    v,
                ),
                Span::raw("  "),
                Span::styled(format!("({:.0}%)", util), hs(util, 60.0, 80.0, false)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Cache        ", l),
                Span::styled(
                    format!("{:.1}%", pg.cache.hit_ratio * 100.0),
                    hs(pg.cache.hit_ratio * 100.0, 95.0, 90.0, true),
                ),
                Span::styled(" hit ratio  ", l),
                Span::styled("Deadlocks: ", l),
                Span::styled(
                    format!("{}", pg.deadlocks),
                    if pg.deadlocks > 0 {
                        Style::default().fg(Color::Red)
                    } else {
                        v
                    },
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Storage      ", l),
                Span::styled(fb(pg.database_size.total_bytes), a),
                Span::styled(" total  ", l),
                Span::styled(fb(pg.database_size.tables_bytes), v),
                Span::styled(" tables  ", l),
                Span::styled(fb(pg.database_size.indexes_bytes), v),
                Span::styled(" indexes", l),
            ]));
            if !pg.index_health.unused_indexes.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("Indexes      ", l),
                    Span::styled(
                        format!("{} unused", pg.index_health.unused_indexes.len()),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        format!(
                            " ({}) / {} total",
                            fb(pg.index_health.unused_bytes),
                            pg.index_health.total_index_count
                        ),
                        l,
                    ),
                    Span::styled("  — consider removing to save space", hint_style),
                ]));
            }
            if !pg.missing_indexes.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{} tables may need an index", pg.missing_indexes.len()),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled("  (high seq scans, no index scans, >1K rows)", hint_style),
                ]));
                for t in pg.missing_indexes.iter().take(3) {
                    lines.push(Line::from(vec![
                        Span::styled("             ", l),
                        Span::styled(truncate_str(&t.table_name, 24), v),
                        Span::styled(
                            format!("  {} rows  {} seq scans", fk(t.live_rows), fk(t.seq_scan)),
                            l,
                        ),
                    ]));
                }
            }
            if !pg.table_stats.is_empty() {
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    "Table                     Size       Seq Scan   Idx Scan   Dead Rows",
                    l,
                )));
                for t in &pg.table_stats {
                    let dead_pct = if t.live_tuples + t.dead_tuples > 0 {
                        t.dead_tuples as f64 / (t.live_tuples + t.dead_tuples) as f64 * 100.0
                    } else {
                        0.0
                    };
                    let name = truncate_str(&t.table_name, 24);
                    lines.push(Line::from(vec![
                        Span::styled(format!("{:<24}", name), v),
                        Span::styled(format!("  {:>8}", fb(t.size_bytes)), v),
                        Span::styled(format!("  {:>9}", fk(t.seq_scan)), v),
                        Span::styled(format!("  {:>9}", fk(t.idx_scan)), v),
                        Span::styled(
                            format!("  {:>7.1}%", dead_pct),
                            hs(dead_pct, 5.0, 10.0, false),
                        ),
                    ]));
                }
            }
            if let Some(ref queries) = pg.query_stats {
                if !queries.is_empty() {
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        "Calls     Total       Mean      Query (pg_stat_statements)",
                        l,
                    )));
                    for q in queries {
                        lines.push(Line::from(vec![
                            Span::styled(format!("{:>7}", fk(q.calls)), v),
                            Span::styled(format!("  {:>9}", fmt_duration(q.total_time_ms)), v),
                            Span::styled(format!("  {:>8}", fmt_duration(q.mean_time_ms)), v),
                            Span::styled(
                                format!("  {}", truncate_str(&q.query, 55)),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]));
                    }
                }
            } else {
                lines.push(Line::default());
                lines.push(Line::from(Span::styled("Query stats unavailable — enable pg_stat_statements to track query performance", hint_style)));
                lines.push(Line::from(Span::styled(
                    "Add pg_stat_statements to shared_preload_libraries and restart the database",
                    hint_style,
                )));
            }
            let needs_vacuum: Vec<_> = pg
                .vacuum_health
                .iter()
                .filter(|t| t.dead_rows_pct > 10.0)
                .collect();
            let needs_freeze: Vec<_> = pg
                .vacuum_health
                .iter()
                .filter(|t| t.xid_age > 150_000_000)
                .collect();
            if !needs_vacuum.is_empty() || !needs_freeze.is_empty() {
                lines.push(Line::default());
                let mut spans = vec![Span::styled("Vacuum       ", l)];
                if !needs_vacuum.is_empty() {
                    spans.push(Span::styled(
                        format!("{} tables need vacuum", needs_vacuum.len()),
                        Style::default().fg(Color::Yellow),
                    ));
                    spans.push(Span::styled("  ", l));
                }
                if !needs_freeze.is_empty() {
                    spans.push(Span::styled(
                        format!("{} need freeze (XID wraparound risk)", needs_freeze.len()),
                        Style::default().fg(Color::Red),
                    ));
                }
                lines.push(Line::from(spans));
            }
        }
        DatabaseStats::Redis(r) => {
            lines.push(Line::from(vec![
                Span::styled("Server       ", l),
                Span::styled(format!("v{}", r.server.version), a),
                Span::styled("  Clients: ", l),
                Span::styled(format!("{}", r.server.connected_clients), v),
                Span::styled("  Blocked: ", l),
                Span::styled(format!("{}", r.server.blocked_clients), v),
                Span::styled("  Ops/sec: ", l),
                Span::styled(format!("{:.0}", r.throughput.ops_per_sec), a),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Memory       ", l),
                Span::styled(fb(r.memory.used_bytes), a),
                Span::styled(" used  ", l),
                Span::styled(fb(r.memory.peak_bytes), v),
                Span::styled(" peak  ", l),
                Span::styled(format!("{:.2}x", r.memory.fragmentation_ratio), v),
                Span::styled(" frag  ", l),
                Span::styled(&r.memory.eviction_policy, v),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Cache        ", l),
                Span::styled(
                    format!("{:.1}%", r.cache.hit_rate * 100.0),
                    hs(r.cache.hit_rate * 100.0, 95.0, 90.0, true),
                ),
                Span::styled(" hit rate  ", l),
                Span::styled(fk(r.cache.hits), v),
                Span::styled(" hits  ", l),
                Span::styled(fk(r.cache.misses), v),
                Span::styled(" misses  ", l),
                Span::styled(fk(r.cache.expired_keys), v),
                Span::styled(" expired", l),
            ]));
            if !r.keyspace.is_empty() {
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    "DB       Keys       Expires    Avg TTL",
                    l,
                )));
                for db in &r.keyspace {
                    lines.push(Line::from(vec![
                        Span::styled(format!("db{:<5}", db.db_index), v),
                        Span::styled(format!("  {:>9}", fk(db.keys)), v),
                        Span::styled(format!("  {:>9}", fk(db.expires)), v),
                        Span::styled(
                            if db.avg_ttl > 0 {
                                format!("  {:>9}", format!("{}ms", fk(db.avg_ttl)))
                            } else {
                                "          -".to_string()
                            },
                            l,
                        ),
                    ]));
                }
            }
        }
        DatabaseStats::MySQL(my) => {
            let util = if my.connections.max_connections > 0 {
                my.connections.threads_connected as f64 / my.connections.max_connections as f64
                    * 100.0
            } else {
                0.0
            };
            lines.push(Line::from(vec![
                Span::styled("Connections  ", l),
                Span::styled(format!("{}", my.connections.threads_connected), a),
                Span::styled(
                    format!(" / {} connected  ", my.connections.max_connections),
                    l,
                ),
                Span::styled(format!("{}", my.connections.threads_running), v),
                Span::styled(" running  ", l),
                Span::styled(format!("({:.0}%)", util), hs(util, 60.0, 80.0, false)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Buffer Pool  ", l),
                Span::styled(
                    format!("{:.1}%", my.buffer_pool.hit_ratio * 100.0),
                    hs(my.buffer_pool.hit_ratio * 100.0, 95.0, 90.0, true),
                ),
                Span::styled(" hit ratio  ", l),
                Span::styled(fb(my.buffer_pool.total_bytes), v),
                Span::styled(format!("  {:.0}% used", my.buffer_pool.usage_pct), l),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Queries      ", l),
                Span::styled(fk(my.queries.selects), v),
                Span::styled(" sel  ", l),
                Span::styled(fk(my.queries.inserts), v),
                Span::styled(" ins  ", l),
                Span::styled(fk(my.queries.updates), v),
                Span::styled(" upd  ", l),
                Span::styled(fk(my.queries.deletes), v),
                Span::styled(" del  ", l),
                Span::styled(
                    format!("{} slow", my.queries.slow_queries),
                    if my.queries.slow_queries > 0 {
                        Style::default().fg(Color::Yellow)
                    } else {
                        l
                    },
                ),
            ]));
        }
        DatabaseStats::MongoDB(m) => {
            lines.push(Line::from(vec![
                Span::styled("Connections  ", l),
                Span::styled(format!("{}", m.connections.current), a),
                Span::styled(format!(" / {} available  ", m.connections.available), l),
                Span::styled(fk(m.connections.total_created), v),
                Span::styled(" total created", l),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Operations   ", l),
                Span::styled(fk(m.operations.query), v),
                Span::styled(" query  ", l),
                Span::styled(fk(m.operations.insert), v),
                Span::styled(" insert  ", l),
                Span::styled(fk(m.operations.update), v),
                Span::styled(" update  ", l),
                Span::styled(fk(m.operations.delete), v),
                Span::styled(" delete  ", l),
                Span::styled(fk(m.operations.command), v),
                Span::styled(" cmd", l),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Memory       ", l),
                Span::styled(format!("{} MB", m.memory.resident_mb), a),
                Span::styled(" resident  ", l),
                Span::styled(format!("{} MB", m.memory.virtual_mb), v),
                Span::styled(" virtual", l),
            ]));
            if m.wired_tiger.cache_max_bytes > 0 {
                let util = m.wired_tiger.cache_used_bytes as f64
                    / m.wired_tiger.cache_max_bytes as f64
                    * 100.0;
                lines.push(Line::from(vec![
                    Span::styled("WT Cache     ", l),
                    Span::styled(fb(m.wired_tiger.cache_used_bytes), v),
                    Span::styled(format!(" / {}", fb(m.wired_tiger.cache_max_bytes)), l),
                    Span::raw("  "),
                    Span::styled(format!("({:.0}%)", util), hs(util, 80.0, 95.0, false)),
                ]));
            }
        }
    }

    // Scrollable: use Paragraph::scroll
    let total_lines = lines.len() as u16;
    let visible = inner.height;
    let max_scroll = total_lines.saturating_sub(visible);

    // Clamp scroll offset
    let scroll = app.db_stats_scroll.min(max_scroll);

    if total_lines > visible {
        // Add scroll hint at the bottom of the block title
        let scroll_hint = format!(" ↑↓ {}/{} ", scroll + 1, max_scroll + 1);
        let hint_span = Span::styled(scroll_hint, Style::default().fg(Color::DarkGray));
        let hint_width = hint_span.width() as u16;
        let hint_area = Rect {
            x: area.x + area.width.saturating_sub(hint_width + 2),
            y: area.y + area.height.saturating_sub(1),
            width: hint_width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(Line::from(hint_span)), hint_area);
    }

    let paragraph = Paragraph::new(lines).scroll((scroll, 0));
    frame.render_widget(paragraph, inner);
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max <= 1 {
        "…".repeat(max)
    } else {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

fn fmt_duration(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.1}s", ms / 1000.0)
    } else if ms >= 1.0 {
        format!("{:.1}ms", ms)
    } else {
        format!("{:.0}µs", ms * 1000.0)
    }
}

fn render_service_help_bar(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let key_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let on_style = Style::default().fg(Color::White);
    let off_style = Style::default().fg(Color::DarkGray);

    let cpu_style = if app.show_cpu { on_style } else { off_style };
    let mem_style = if app.show_memory { on_style } else { off_style };
    let net_style = if app.show_network {
        on_style
    } else {
        off_style
    };
    let vol_style = if app.show_volume { on_style } else { off_style };
    let http_style = if app.show_http && !app.is_db {
        on_style
    } else {
        off_style
    };

    let help = Line::from(vec![
        Span::raw(" "),
        Span::styled("1", key_style),
        Span::styled(" cpu ", cpu_style),
        Span::styled("2", key_style),
        Span::styled(" mem ", mem_style),
        Span::styled("3", key_style),
        Span::styled(" net ", net_style),
        Span::styled("4", key_style),
        Span::styled(" vol ", vol_style),
        Span::styled("5", key_style),
        Span::styled(" http ", http_style),
        Span::styled(" · ", Style::default().fg(LABEL_COLOR)),
        Span::styled(if app.db_stats_supported { "Tab" } else { "" }, key_style),
        Span::styled(
            if app.db_stats_supported {
                " switch view "
            } else {
                ""
            },
            Style::default().fg(LABEL_COLOR),
        ),
        Span::styled("t", key_style),
        Span::styled(
            format!(" {} ", app.time_range_label()),
            Style::default().fg(Color::White),
        ),
        Span::styled("r", key_style),
        Span::styled(" refresh ", Style::default().fg(LABEL_COLOR)),
        Span::styled("?", key_style),
        Span::styled(" help ", Style::default().fg(LABEL_COLOR)),
        Span::styled("q", key_style),
        Span::styled(" quit", Style::default().fg(LABEL_COLOR)),
    ]);

    frame.render_widget(Paragraph::new(help), area);
}

// ─── Project-wide rendering ──────────────────────────────────────────────────

pub fn render_project(app: &ProjectApp, frame: &mut Frame) {
    let area = frame.area();

    if area.width < 60 || area.height < 20 {
        let msg = Paragraph::new("Terminal too small. Please resize (min 60x20).")
            .style(Style::default().fg(Color::Yellow));
        frame.render_widget(msg, area);
        return;
    }

    let max_table_body = (area.height / 3).max(3);
    let table_body_rows = (app.services.len() as u16).min(max_table_body).max(1);

    let chunks = Layout::vertical([
        Constraint::Length(2),               // header
        Constraint::Length(2),               // table header
        Constraint::Length(table_body_rows), // table body (scrollable)
        Constraint::Min(10),                 // detail panel
        Constraint::Length(1),               // help bar
    ])
    .spacing(CARD_SPACING)
    .split(area);

    render_project_header(app, frame, chunks[0]);
    render_table_header(frame, chunks[1]);
    render_scrollable_services_table(app, frame, chunks[2]);
    render_detail_panel(app, frame, chunks[3]);
    render_project_help_bar(app, frame, chunks[4]);

    if let Some(ref err) = app.error_message {
        let err_area = Rect {
            y: chunks[3].y,
            height: 1,
            ..chunks[3]
        };
        let err_line = Paragraph::new(format!(" {err}")).style(Style::default().fg(Color::Red));
        frame.render_widget(err_line, err_area);
    }

    if app.show_help {
        render_help_overlay(frame, area);
    }
}

fn render_project_header(app: &ProjectApp, frame: &mut Frame, area: Rect) {
    let refresh_str = app
        .last_refresh
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "loading...".to_string());

    let header = Line::from(vec![
        Span::styled(
            format!("  {}", app.project_name),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::default().fg(LABEL_COLOR)),
        Span::raw(format!("env {}", app.environment_name)),
        Span::styled("  ·  ", Style::default().fg(LABEL_COLOR)),
        Span::raw(format!("last {}", app.time_range_label())),
        Span::styled("  ·  ", Style::default().fg(LABEL_COLOR)),
        Span::styled(
            format!("refreshed {refresh_str}"),
            Style::default().fg(LABEL_COLOR),
        ),
        if app.refreshing {
            Span::styled("  ·  refreshing", Style::default().fg(Color::Yellow))
        } else {
            Span::raw("")
        },
        Span::styled("  ·  ", Style::default().fg(LABEL_COLOR)),
        Span::raw(format!("{} services", app.services.len())),
    ]);

    frame.render_widget(Paragraph::new(vec![header, Line::from("")]), area);
}

fn render_table_header(frame: &mut Frame, area: Rect) {
    let header_style = Style::default()
        .fg(LABEL_COLOR)
        .add_modifier(Modifier::BOLD);

    let header = Line::from(vec![
        Span::styled(format!("    {:<22}", "Service"), header_style),
        Span::styled(format!("{:<16}", "CPU"), header_style),
        Span::styled(format!("{:<16}", "Memory"), header_style),
        Span::styled(format!("{:<10}", "Disk"), header_style),
        Span::styled(format!("{:<10}", "Reqs"), header_style),
        Span::styled(format!("{:<8}", "Err%"), header_style),
        Span::styled("p50", header_style),
    ]);
    let separator = Line::from(format!(
        "  {}",
        "─".repeat(area.width.saturating_sub(3) as usize)
    ));

    frame.render_widget(Paragraph::new(vec![header, separator]), area);
}

fn render_scrollable_services_table(app: &ProjectApp, frame: &mut Frame, area: Rect) {
    if app.services.is_empty() {
        let msg = Paragraph::new("  No services found.").style(Style::default().fg(LABEL_COLOR));
        frame.render_widget(msg, area);
        return;
    }

    let visible_rows = area.height as usize;
    let start = app.table_scroll_offset;
    let end = (start + visible_rows).min(app.services.len());

    let constraints: Vec<Constraint> = (start..end)
        .map(|_| Constraint::Length(1))
        .chain(std::iter::once(Constraint::Min(0)))
        .collect();
    let rows = Layout::vertical(constraints).split(area);

    for (row_idx, svc_idx) in (start..end).enumerate() {
        let svc = &app.services[svc_idx];
        let is_selected = svc_idx == app.selected_idx;

        // Service name with DB indicator
        let db_tag = if svc.is_database { " ◆" } else { "" };
        let col_width = 20usize;
        let tag_len = db_tag.chars().count();
        let avail = col_width.saturating_sub(tag_len);
        let raw = format!("{}{}", truncate_str(&svc.service_name, avail), db_tag);
        let name = format!("{:<col_width$}", raw);

        let cpu_str = svc
            .cpu
            .as_ref()
            .map(|c| {
                let val = format_cpu(c.current);
                let color = svc
                    .cpu_limit
                    .as_ref()
                    .filter(|l| l.current > 0.0)
                    .and_then(|l| utilization(c.current, Some(l.current)))
                    .map(health_color)
                    .unwrap_or(Color::White);
                (val, color)
            })
            .unwrap_or(("—".to_string(), LABEL_COLOR));

        let mem_str = svc
            .memory
            .as_ref()
            .map(|m| {
                let val = format_gb(m.current);
                let color = svc
                    .memory_limit
                    .as_ref()
                    .filter(|l| l.current > 0.0)
                    .and_then(|l| utilization(m.current, Some(l.current)))
                    .map(health_color)
                    .unwrap_or(Color::White);
                (val, color)
            })
            .unwrap_or(("—".to_string(), LABEL_COLOR));

        let disk_str = if let Some(vol) = svc.volumes.first() {
            format_mb(vol.current_size_mb)
        } else {
            "—".to_string()
        };

        let (reqs_str, err_str, err_color, p50_str) = if svc.is_database {
            ("—".into(), "—".into(), LABEL_COLOR, "—".into())
        } else if let Some(ref http) = svc.http {
            (
                format_count(http.total),
                format!("{:.1}%", http.error_rate),
                error_rate_color(http.error_rate),
                format!("{}ms", http.p50_ms),
            )
        } else {
            ("—".into(), "—".into(), LABEL_COLOR, "—".into())
        };

        let cursor = if is_selected { "▸" } else { " " };
        let name_style = if is_selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let line = Line::from(vec![
            Span::styled(format!(" {cursor} "), Style::default().fg(Color::Yellow)),
            Span::styled(format!("{:<22}", name), name_style),
            Span::styled(format!("{:<16}", cpu_str.0), Style::default().fg(cpu_str.1)),
            Span::styled(format!("{:<16}", mem_str.0), Style::default().fg(mem_str.1)),
            Span::styled(
                format!("{:<10}", disk_str),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                format!("{:<10}", reqs_str),
                Style::default().fg(Color::White),
            ),
            Span::styled(format!("{:<8}", err_str), Style::default().fg(err_color)),
            Span::styled(p50_str, Style::default().fg(Color::White)),
        ]);

        frame.render_widget(Paragraph::new(line), rows[row_idx]);
    }

    // Scroll indicators
    if start > 0 {
        let indicator = Span::styled(" ▴", Style::default().fg(LABEL_COLOR));
        let r = Rect {
            x: area.x + area.width.saturating_sub(3),
            y: area.y,
            width: 2,
            height: 1,
        };
        frame.render_widget(Paragraph::new(Line::from(indicator)), r);
    }
    if end < app.services.len() {
        let indicator = Span::styled(" ▾", Style::default().fg(LABEL_COLOR));
        let r = Rect {
            x: area.x + area.width.saturating_sub(3),
            y: area.y + area.height.saturating_sub(1),
            width: 2,
            height: 1,
        };
        frame.render_widget(Paragraph::new(Line::from(indicator)), r);
    }
}

fn render_detail_panel(app: &ProjectApp, frame: &mut Frame, area: Rect) {
    let svc = match app.selected_service() {
        Some(s) => s,
        None => {
            let msg =
                Paragraph::new("  No service selected").style(Style::default().fg(LABEL_COLOR));
            frame.render_widget(msg, area);
            return;
        }
    };

    let detail = match app.detail_cache.get(&svc.service_id) {
        Some(entry) if entry.time_range_idx == app.time_range_idx => entry,
        _ => {
            let msg = Paragraph::new(format!("  Loading {}...", svc.service_name))
                .style(Style::default().fg(LABEL_COLOR));
            frame.render_widget(msg, area);
            return;
        }
    };

    // Split: service name header + chart area
    let parts = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);

    let db_label = if svc.is_database { " (database)" } else { "" };
    let detail_header = Line::from(vec![Span::styled(
        format!("  {}{db_label}", svc.service_name),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]);
    frame.render_widget(Paragraph::new(detail_header), parts[0]);

    let temp_app = MetricsApp {
        service_name: svc.service_name.clone(),
        environment_name: app.environment_name.clone(),
        is_db: detail.is_database,
        db_stats_supported: false,
        active_tab: ActiveTab::Metrics,
        show_cpu: app.show_cpu,
        show_memory: app.show_memory,
        show_network: app.show_network,
        show_volume: app.show_volume,
        show_http: app.show_http,
        show_egress: app.show_egress,
        show_ingress: app.show_ingress,
        show_2xx: app.show_2xx,
        show_3xx: app.show_3xx,
        show_4xx: app.show_4xx,
        show_5xx: app.show_5xx,
        show_p50: app.show_p50,
        show_p90: app.show_p90,
        show_p95: app.show_p95,
        show_p99: app.show_p99,
        time_range_idx: app.time_range_idx,
        time_range_changed: false,
        cpu: detail.cpu.clone(),
        cpu_limit: detail.cpu_limit.clone(),
        memory: detail.memory.clone(),
        memory_limit: detail.memory_limit.clone(),
        network_tx: detail.network_tx.clone(),
        network_rx: detail.network_rx.clone(),
        disk: detail.disk.clone(),
        volumes: detail.volumes.clone(),
        http: detail.http.clone(),
        db_stats: None,
        db_stats_error: None,
        db_stats_handle: None,
        db_stats_scroll: 0,
        last_refresh: Some(detail.fetched_at),
        error_message: None,
        show_help: false,
        force_refresh: false,
        refreshing: false,
    };

    render_detail_charts(&temp_app, frame, parts[1]);
}

/// Render the chart dashboard for a service into a sub-area.
/// Extracted from render_service() — same layout minus header and help bar.
fn render_detail_charts(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let show_cpu_mem = app.show_cpu || app.show_memory;
    let show_net_req = app.show_network || (app.show_http && !app.is_db);
    let show_err_resp = app.show_http && !app.is_db;
    let show_vol = app.show_volume && (!app.volumes.is_empty() || app.disk.is_some());

    let mut constraints: Vec<Constraint> = vec![];
    let mut chart_rows = 0u32;
    if show_cpu_mem {
        chart_rows += 1;
    }
    if show_net_req {
        chart_rows += 1;
    }
    if show_err_resp {
        chart_rows += 1;
    }
    let chart_rows = chart_rows.max(1);

    if show_cpu_mem {
        constraints.push(Constraint::Ratio(1, chart_rows));
    }
    if show_net_req {
        constraints.push(Constraint::Ratio(1, chart_rows));
    }
    if show_err_resp {
        constraints.push(Constraint::Ratio(1, chart_rows));
    }
    if show_vol {
        constraints.push(Constraint::Length(6)); // border(2) + padding(2) + gauge(1) + stats(1)
    }
    constraints.push(Constraint::Min(0)); // filler

    let chunks = Layout::vertical(constraints)
        .spacing(CARD_SPACING)
        .split(area);

    let mut idx = 0;
    if show_cpu_mem {
        render_cpu_memory(app, frame, chunks[idx]);
        idx += 1;
    }
    if show_net_req {
        render_network_http(app, frame, chunks[idx]);
        idx += 1;
    }
    if show_err_resp {
        let row_cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .spacing(CARD_SPACING)
            .split(chunks[idx]);
        render_error_rate_chart(app, frame, row_cols[0]);
        render_http_latency(app, frame, row_cols[1]);
        idx += 1;
    }
    if show_vol {
        render_volume(app, frame, chunks[idx]);
    }
}

fn render_project_help_bar(app: &ProjectApp, frame: &mut Frame, area: Rect) {
    let key_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let on_style = Style::default().fg(Color::White);
    let off_style = Style::default().fg(Color::DarkGray);

    let cpu_style = if app.show_cpu { on_style } else { off_style };
    let mem_style = if app.show_memory { on_style } else { off_style };
    let net_style = if app.show_network {
        on_style
    } else {
        off_style
    };
    let http_style = if app.show_http { on_style } else { off_style };

    let help = Line::from(vec![
        Span::raw(" "),
        Span::styled("j/k", key_style),
        Span::styled(" nav ", Style::default().fg(LABEL_COLOR)),
        Span::styled("1", key_style),
        Span::styled(" cpu ", cpu_style),
        Span::styled("2", key_style),
        Span::styled(" mem ", mem_style),
        Span::styled("3", key_style),
        Span::styled(" net ", net_style),
        Span::styled("5", key_style),
        Span::styled(" http ", http_style),
        Span::styled(" · ", Style::default().fg(LABEL_COLOR)),
        Span::styled("t", key_style),
        Span::styled(
            format!(" {} ", app.time_range_label()),
            Style::default().fg(Color::White),
        ),
        Span::styled("r", key_style),
        Span::styled(" refresh ", Style::default().fg(LABEL_COLOR)),
        Span::styled("?", key_style),
        Span::styled(" help ", Style::default().fg(LABEL_COLOR)),
        Span::styled("q", key_style),
        Span::styled(" quit", Style::default().fg(LABEL_COLOR)),
    ]);

    frame.render_widget(Paragraph::new(help), area);
}

// ─── Shared widgets ──────────────────────────────────────────────────────────

fn render_help_overlay(frame: &mut Frame, area: Rect) {
    let width = 52u16.min(area.width.saturating_sub(4));
    let height = 26u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let overlay = Rect::new(x, y, width, height);

    let clear = Paragraph::new(vec![Line::from(""); height as usize])
        .style(Style::default().bg(Color::Black));
    frame.render_widget(clear, overlay);

    let block = Block::default()
        .title(" Help ")
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);

    let key_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let section = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);

    let lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled("  Navigation", section)]),
        Line::from(vec![
            Span::styled("  j/k ↑↓ ", key_style),
            Span::raw("Select service"),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled("  Sections", section)]),
        Line::from(vec![
            Span::styled("  1-5    ", key_style),
            Span::raw("Toggle metric sections"),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled("  Network filters", section)]),
        Line::from(vec![
            Span::styled("  e      ", key_style),
            Span::raw("Toggle Egress"),
        ]),
        Line::from(vec![
            Span::styled("  i      ", key_style),
            Span::raw("Toggle Ingress"),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled("  HTTP status filters", section)]),
        Line::from(vec![
            Span::styled("  6-9    ", key_style),
            Span::raw("Toggle 2xx / 3xx / 4xx / 5xx"),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled("  Response time filters", section)]),
        Line::from(vec![
            Span::styled("  F1-F4  ", key_style),
            Span::raw("Toggle p50 / p90 / p95 / p99"),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled("  General", section)]),
        Line::from(vec![
            Span::styled("  t      ", key_style),
            Span::raw("Cycle time range (1h/6h/1d/7d/30d)"),
        ]),
        Line::from(vec![
            Span::styled("  r      ", key_style),
            Span::raw("Force refresh now"),
        ]),
        Line::from(vec![
            Span::styled("  q/Esc  ", key_style),
            Span::raw("Quit"),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Press any key to close",
            Style::default().fg(LABEL_COLOR),
        )]),
    ];

    frame.render_widget(Paragraph::new(lines), inner);
}
