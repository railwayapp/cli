use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

use crate::{
    controllers::regions::{
        region_display_name, region_flag_name, region_full_label, region_is_available,
    },
    gql::queries,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaleTuiAction {
    Continue,
    Apply(HashMap<String, u64>),
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleTuiMode {
    Browse,
    Search,
    Edit,
    Confirm,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionRow {
    pub name: String,
    pub cli_name: String,
    pub label: String,
    pub current: u64,
    pub desired: u64,
    pub available: bool,
    pub dedicated: bool,
}

impl RegionRow {
    pub fn change(&self) -> i128 {
        self.desired as i128 - self.current as i128
    }

    pub fn changed(&self) -> bool {
        self.current != self.desired
    }
}

#[derive(Debug)]
pub struct ScaleTuiApp {
    pub service_name: String,
    pub environment_name: String,
    pub rows: Vec<RegionRow>,
    pub selected: usize,
    pub mode: ScaleTuiMode,
    pub search: String,
    pub edit_input: String,
    pub error: Option<String>,
}

impl ScaleTuiApp {
    pub fn new(
        service_name: String,
        environment_name: String,
        regions: queries::regions::ResponseData,
        existing: &Value,
    ) -> Self {
        let current = current_replicas(existing);
        let mut seen = HashSet::new();
        let mut rows = regions
            .regions
            .iter()
            .filter_map(|region| {
                let replicas = *current.get(&region.name).unwrap_or(&0);
                let available = region_is_available(region);
                if !available && replicas == 0 {
                    return None;
                }

                seen.insert(region.name.clone());
                Some(RegionRow {
                    name: region.name.clone(),
                    cli_name: region_flag_name(region),
                    label: region_full_label(region),
                    current: replicas,
                    desired: replicas,
                    available,
                    dedicated: region.workspace_id.is_some(),
                })
            })
            .collect::<Vec<_>>();

        for (name, replicas) in current {
            if seen.contains(&name) || replicas == 0 {
                continue;
            }

            rows.push(RegionRow {
                cli_name: name.clone(),
                label: region_display_name(&name, &HashMap::new()),
                name,
                current: replicas,
                desired: replicas,
                available: false,
                dedicated: false,
            });
        }

        rows.sort_by(|a, b| {
            b.current
                .cmp(&a.current)
                .then_with(|| a.label.cmp(&b.label))
                .then_with(|| a.name.cmp(&b.name))
        });

        Self {
            service_name,
            environment_name,
            rows,
            selected: 0,
            mode: ScaleTuiMode::Browse,
            search: String::new(),
            edit_input: String::new(),
            error: None,
        }
    }

    pub fn visible_indices(&self) -> Vec<usize> {
        let query = self.search.trim().to_ascii_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(idx, row)| {
                if query.is_empty()
                    || row.label.to_ascii_lowercase().contains(&query)
                    || row.cli_name.to_ascii_lowercase().contains(&query)
                    || row.name.to_ascii_lowercase().contains(&query)
                {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn selected_row(&self) -> Option<&RegionRow> {
        let visible = self.visible_indices();
        visible
            .get(self.selected)
            .and_then(|idx| self.rows.get(*idx))
    }

    pub fn changes(&self) -> HashMap<String, u64> {
        self.rows
            .iter()
            .filter(|row| row.changed())
            .map(|row| (row.name.clone(), row.desired))
            .collect()
    }

    pub fn changed_rows(&self) -> Vec<&RegionRow> {
        self.rows.iter().filter(|row| row.changed()).collect()
    }

    pub fn command_preview(&self) -> String {
        let mut changes = self.changed_rows();
        changes.sort_by(|a, b| a.cli_name.cmp(&b.cli_name));

        if changes.is_empty() {
            return "No changes yet".to_string();
        }

        let mut parts = vec![
            "railway".to_string(),
            "scale".to_string(),
            "--environment".to_string(),
            shell_arg(&self.environment_name),
            "--service".to_string(),
            shell_arg(&self.service_name),
        ];
        parts.extend(
            changes
                .iter()
                .map(|row| format!("{}={}", self.command_region_name(row), row.desired)),
        );
        parts.join(" ")
    }

    fn command_region_name<'a>(&'a self, row: &'a RegionRow) -> &'a str {
        let duplicates = self
            .rows
            .iter()
            .filter(|candidate| candidate.cli_name == row.cli_name)
            .count();
        if row.cli_name.is_empty() || duplicates > 1 {
            &row.name
        } else {
            &row.cli_name
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ScaleTuiAction {
        self.error = None;

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return ScaleTuiAction::Cancel;
        }

        match self.mode {
            ScaleTuiMode::Browse => self.handle_browse_key(key),
            ScaleTuiMode::Search => self.handle_search_key(key),
            ScaleTuiMode::Edit => self.handle_edit_key(key),
            ScaleTuiMode::Confirm => self.handle_confirm_key(key),
            ScaleTuiMode::Help => self.handle_help_key(key),
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> ScaleTuiAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ScaleTuiAction::Cancel,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                ScaleTuiAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                ScaleTuiAction::Continue
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.selected = 0;
                ScaleTuiAction::Continue
            }
            KeyCode::End | KeyCode::Char('G') => {
                let len = self.visible_indices().len();
                self.selected = len.saturating_sub(1);
                ScaleTuiAction::Continue
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.adjust_selected(1);
                ScaleTuiAction::Continue
            }
            KeyCode::Char('-') => {
                self.adjust_selected(-1);
                ScaleTuiAction::Continue
            }
            KeyCode::Char('0') => {
                if let Some(row) = self.selected_row_mut() {
                    row.desired = 0;
                }
                ScaleTuiAction::Continue
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() => {
                self.edit_input = ch.to_string();
                self.mode = ScaleTuiMode::Edit;
                ScaleTuiAction::Continue
            }
            KeyCode::Enter | KeyCode::Char('e') => {
                if let Some(row) = self.selected_row() {
                    self.edit_input = row.desired.to_string();
                    self.mode = ScaleTuiMode::Edit;
                }
                ScaleTuiAction::Continue
            }
            KeyCode::Char('/') => {
                self.mode = ScaleTuiMode::Search;
                ScaleTuiAction::Continue
            }
            KeyCode::Char('a') => {
                if self.changes().is_empty() {
                    return ScaleTuiAction::Apply(HashMap::new());
                }
                self.mode = ScaleTuiMode::Confirm;
                ScaleTuiAction::Continue
            }
            KeyCode::Char('?') => {
                self.mode = ScaleTuiMode::Help;
                ScaleTuiAction::Continue
            }
            _ => ScaleTuiAction::Continue,
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> ScaleTuiAction {
        match key.code {
            KeyCode::Esc => {
                if self.search.is_empty() {
                    self.mode = ScaleTuiMode::Browse;
                } else {
                    self.search.clear();
                    self.selected = 0;
                }
            }
            KeyCode::Enter => self.mode = ScaleTuiMode::Browse,
            KeyCode::Backspace => {
                self.search.pop();
                self.clamp_selection();
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.push(ch);
                self.selected = 0;
                self.clamp_selection();
            }
            _ => {}
        }
        ScaleTuiAction::Continue
    }

    fn handle_edit_key(&mut self, key: KeyEvent) -> ScaleTuiAction {
        match key.code {
            KeyCode::Esc => {
                self.mode = ScaleTuiMode::Browse;
                self.edit_input.clear();
            }
            KeyCode::Enter => match self.edit_input.parse::<u64>() {
                Ok(replicas) => {
                    if let Some(row) = self.selected_row_mut() {
                        row.desired = replicas;
                    }
                    self.mode = ScaleTuiMode::Browse;
                    self.edit_input.clear();
                }
                Err(_) => {
                    self.error = Some("Replica count must be a whole number.".to_string());
                }
            },
            KeyCode::Backspace => {
                self.edit_input.pop();
            }
            KeyCode::Delete => {
                self.edit_input.clear();
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() => {
                self.edit_input.push(ch);
            }
            _ => {}
        }
        ScaleTuiAction::Continue
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> ScaleTuiAction {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('a') => {
                ScaleTuiAction::Apply(self.changes())
            }
            KeyCode::Esc | KeyCode::Char('e') => {
                self.mode = ScaleTuiMode::Browse;
                ScaleTuiAction::Continue
            }
            KeyCode::Char('q') | KeyCode::Char('n') => ScaleTuiAction::Cancel,
            _ => ScaleTuiAction::Continue,
        }
    }

    fn handle_help_key(&mut self, key: KeyEvent) -> ScaleTuiAction {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q') => {
                self.mode = ScaleTuiMode::Browse;
            }
            _ => {}
        }
        ScaleTuiAction::Continue
    }

    fn selected_row_mut(&mut self) -> Option<&mut RegionRow> {
        let visible = self.visible_indices();
        let selected = *visible.get(self.selected)?;
        self.rows.get_mut(selected)
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.visible_indices().len();
        if len == 0 {
            self.selected = 0;
            return;
        }

        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, len.saturating_sub(1) as isize) as usize;
    }

    fn adjust_selected(&mut self, delta: i64) {
        if let Some(row) = self.selected_row_mut() {
            if delta.is_negative() {
                row.desired = row.desired.saturating_sub(delta.unsigned_abs());
            } else {
                row.desired = row.desired.saturating_add(delta as u64);
            }
        }
    }

    fn clamp_selection(&mut self) {
        let len = self.visible_indices().len();
        self.selected = self.selected.min(len.saturating_sub(1));
    }
}

fn current_replicas(existing: &Value) -> HashMap<String, u64> {
    existing
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(name, value)| {
                    let replicas = value
                        .get("numReplicas")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    (name.clone(), replicas)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn region(
        name: &str,
        location: &str,
        country: &str,
        provider_region: Option<&str>,
        is_deprecated: bool,
    ) -> queries::regions::RegionsRegions {
        queries::regions::RegionsRegions {
            name: name.to_string(),
            region: provider_region.map(ToString::to_string),
            country: country.to_string(),
            location: location.to_string(),
            workspace_id: None,
            deployment_constraints: Some(queries::regions::RegionsRegionsDeploymentConstraints {
                deprecation_info: Some(
                    queries::regions::RegionsRegionsDeploymentConstraintsDeprecationInfo {
                        is_deprecated,
                        replacement_region: "us-west2".to_string(),
                    },
                ),
            }),
        }
    }

    #[test]
    fn app_hides_deprecated_regions_unless_currently_scaled() {
        let regions = queries::regions::ResponseData {
            regions: vec![
                region("us-west2", "US West", "US", Some("us-west2"), false),
                region("old-region", "Old Region", "US", None, true),
            ],
        };

        let app = ScaleTuiApp::new(
            "worker".to_string(),
            "production".to_string(),
            regions,
            &json!({"old-region": {"numReplicas": 1}}),
        );

        assert!(app.rows.iter().any(|row| row.name == "old-region"));
        assert!(
            app.rows
                .iter()
                .find(|row| row.name == "old-region")
                .is_some_and(|row| !row.available)
        );
    }

    #[test]
    fn changes_include_only_edited_regions() {
        let regions = queries::regions::ResponseData {
            regions: vec![
                region("us-west2", "US West", "US", Some("us-west2"), false),
                region(
                    "europe-west4-drams3a",
                    "EU West",
                    "NL",
                    Some("europe-west4"),
                    false,
                ),
            ],
        };
        let mut app = ScaleTuiApp::new(
            "worker".to_string(),
            "production".to_string(),
            regions,
            &json!({"us-west2": {"numReplicas": 1}}),
        );

        let eu_west = app
            .rows
            .iter_mut()
            .find(|row| row.name == "europe-west4-drams3a")
            .unwrap();
        eu_west.desired = 2;

        assert_eq!(
            app.changes(),
            HashMap::from([("europe-west4-drams3a".to_string(), 2)])
        );
        assert_eq!(
            app.command_preview(),
            "railway scale --environment production --service worker eu-west=2"
        );
    }

    #[test]
    fn command_preview_uses_region_id_when_cli_name_is_ambiguous() {
        let regions = queries::regions::ResponseData {
            regions: vec![
                region("region-a", "EU West", "NL", Some("europe-west4"), false),
                region("region-b", "EU West", "NL", Some("europe-west4"), false),
            ],
        };
        let mut app = ScaleTuiApp::new(
            "web worker".to_string(),
            "production".to_string(),
            regions,
            &json!({}),
        );

        app.rows
            .iter_mut()
            .find(|row| row.name == "region-a")
            .unwrap()
            .desired = 2;

        assert_eq!(
            app.command_preview(),
            "railway scale --environment production --service 'web worker' region-a=2"
        );
    }

    #[test]
    fn typing_digit_starts_inline_edit_without_changing_until_enter() {
        let regions = queries::regions::ResponseData {
            regions: vec![region("us-west2", "US West", "US", Some("us-west2"), false)],
        };
        let mut app = ScaleTuiApp::new(
            "worker".to_string(),
            "production".to_string(),
            regions,
            &json!({"us-west2": {"numReplicas": 1}}),
        );

        assert_eq!(
            app.handle_key(KeyEvent::from(KeyCode::Char('4'))),
            ScaleTuiAction::Continue
        );
        assert_eq!(app.mode, ScaleTuiMode::Edit);
        assert_eq!(app.edit_input, "4");
        assert_eq!(app.rows[0].desired, 1);

        assert_eq!(
            app.handle_key(KeyEvent::from(KeyCode::Enter)),
            ScaleTuiAction::Continue
        );
        assert_eq!(app.mode, ScaleTuiMode::Browse);
        assert_eq!(app.rows[0].desired, 4);
    }

    #[test]
    fn escape_cancels_inline_edit() {
        let regions = queries::regions::ResponseData {
            regions: vec![region("us-west2", "US West", "US", Some("us-west2"), false)],
        };
        let mut app = ScaleTuiApp::new(
            "worker".to_string(),
            "production".to_string(),
            regions,
            &json!({"us-west2": {"numReplicas": 1}}),
        );

        let _ = app.handle_key(KeyEvent::from(KeyCode::Char('4')));
        let _ = app.handle_key(KeyEvent::from(KeyCode::Esc));

        assert_eq!(app.mode, ScaleTuiMode::Browse);
        assert_eq!(app.edit_input, "");
        assert_eq!(app.rows[0].desired, 1);
    }
}
