//! SSH output invalidates reported metadata, never VM history. The query reads
//! Redis-backed control-plane snapshots and does not dial the machine.
use super::{app::App, client_sessions::Thread};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

const QUIET: Duration = Duration::from_secs(2);
const MAX_WAIT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(super) struct Activity {
    pending: HashMap<String, (Instant, Instant)>,
    inflight: HashSet<String>,
}

impl Activity {
    pub fn changed(&mut self, id: &str, now: Instant) {
        self.pending
            .entry(id.into())
            .and_modify(|entry| entry.1 = now)
            .or_insert((now, now));
    }
    pub fn next(&self, now: Instant) -> Option<Duration> {
        self.pending
            .iter()
            .filter(|(id, _)| !self.inflight.contains(*id))
            .map(|(_, (first, last))| {
                (*first + MAX_WAIT)
                    .min(*last + QUIET)
                    .saturating_duration_since(now)
            })
            .min()
    }
    pub fn take_due(&mut self, now: Instant) -> Vec<String> {
        let ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(id, (first, last))| {
                !self.inflight.contains(*id) && now >= (*first + MAX_WAIT).min(*last + QUIET)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            self.pending.remove(id);
            self.inflight.insert(id.clone());
        }
        ids
    }
    pub fn finished(&mut self, id: &str) {
        self.inflight.remove(id);
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Report {
    harness: String,
    session_id: String,
    session_name: Option<String>,
    state: String,
    title: Option<String>,
    prompt: Option<String>,
    latest_prompt: Option<String>,
    updated_at: String,
}

pub(super) async fn fetch(
    client: &reqwest::Client,
    backboard: &str,
    agent: &str,
    environment: &str,
) -> anyhow::Result<Vec<Report>> {
    #[derive(Deserialize)]
    struct Agent {
        sessions: Vec<Report>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Response {
        cloud_agent: Option<Agent>,
    }
    let res: Response = crate::client::post_graphql_raw(client, backboard,
        "query ReportedConversations($agent: ID!, $environment: ID!) { cloudAgent(id: $agent, environmentId: $environment) { sessions { harness sessionId sessionName state title prompt latestPrompt updatedAt } } }",
        serde_json::json!({"agent":agent,"environment":environment})).await?;
    Ok(res
        .cloud_agent
        .map(|agent| agent.sessions)
        .unwrap_or_default())
}

pub(super) fn apply(app: &mut App, agent: &str, reports: &[Report]) {
    let mut updates = Vec::new();
    for pane in &mut app.sessions {
        if pane.agent_id != agent
            || pane.ended()
            || pane.client_bridge.is_some()
            || pane.opencode_bridge.is_some()
        {
            continue;
        }
        pane.sync_console_name();
        let Some(client_id) = &pane.client_id else {
            continue;
        };
        let report = reports
            .iter()
            .filter(|report| {
                let harness = if report.harness == "railway-agent" {
                    "railway"
                } else {
                    &report.harness
                };
                harness == pane.harness
                    && report.session_name.is_some()
                    && report.session_name == pane.console_name
            })
            .max_by_key(|report| &report.updated_at);
        let Some(report) = report else {
            continue;
        };
        if super::client_sessions::validate_id(&report.session_id).is_err() {
            continue;
        }
        let mut thread = pane
            .client_thread
            .clone()
            .filter(|t| t.id == report.session_id)
            .unwrap_or(Thread {
                id: report.session_id.clone(),
                title: super::client_sessions::NEW_THREAD.into(),
                directory: "/app".into(),
                created_at: None,
                updated_at: String::new(),
                state: "idle".into(),
            });
        let fallback = if thread.title == super::client_sessions::NEW_THREAD {
            report.latest_prompt.as_ref().or(report.prompt.as_ref())
        } else {
            None
        };
        if let Some(title) = report
            .title
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .or(fallback.filter(|s| !s.trim().is_empty()))
        {
            thread.title = super::client_sessions::title(Some(title), &thread.title);
        }
        thread.updated_at = report.updated_at.clone();
        thread.state = report.state.clone();
        updates.push((client_id.clone(), thread));
    }
    for (id, thread) in updates {
        app.client_thread_selected(&id, thread);
    }
    app.persist_threads(agent);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_promote_only_the_exact_live_console_and_keep_its_native_id() {
        let mut app = App::new(Vec::new(), None, Some("claude"), None, None, true);
        for name in ["console-one", "console-two"] {
            let mut pane = super::super::session::Session::for_test("vm", "VM").unwrap();
            pane.harness = "claude".into();
            pane.console_name = Some(name.into());
            pane.client_id = Some(name.into());
            app.sessions.push(pane);
        }
        let report = |console: Option<&str>, id: &str| Report {
            harness: "claude".into(),
            session_id: id.into(),
            session_name: console.map(str::to_owned),
            state: "idle".into(),
            title: Some("Generated weather title".into()),
            prompt: Some("Initial prompt".into()),
            latest_prompt: None,
            updated_at: "2026-09-10T12:00:00Z".into(),
        };
        apply(
            &mut app,
            "vm",
            &[
                report(None, "unattributed"),
                report(Some("console-two"), "native-two"),
            ],
        );
        assert!(app.sessions[0].client_thread.is_none());
        assert_eq!(
            app.sessions[1].client_thread.as_ref().unwrap().id,
            "native-two"
        );
        assert_eq!(
            app.sessions[1].client_thread.as_ref().unwrap().title,
            "Generated weather title"
        );
        apply(
            &mut app,
            "different-vm",
            &[report(Some("console-one"), "wrong-vm")],
        );
        assert!(app.sessions[0].client_thread.is_none());
        apply(&mut app, "vm", &[]);
        let mut untitled = report(Some("console-two"), "native-two");
        untitled.title = None;
        apply(&mut app, "vm", &[untitled]);
        assert_eq!(
            app.sessions[1].client_thread.as_ref().unwrap().title,
            "Generated weather title"
        );
        assert_eq!(
            app.sessions[1].client_thread.as_ref().unwrap().id,
            "native-two"
        );
    }

    #[test]
    fn output_is_coalesced_and_completion_never_rearms_idle_work() {
        let mut activity = Activity::default();
        let now = Instant::now();
        assert_eq!(activity.next(now), None);
        for second in 0..10 {
            activity.changed("vm", now + Duration::from_secs(second));
        }
        assert_eq!(activity.take_due(now + MAX_WAIT), vec!["vm"]);
        assert_eq!(activity.next(now + MAX_WAIT), None);
        activity.finished("vm");
        assert!(
            activity
                .take_due(now + Duration::from_secs(3600))
                .is_empty()
        );
        activity.changed("vm", now + MAX_WAIT);
        assert_eq!(activity.take_due(now + MAX_WAIT + QUIET), vec!["vm"]);
        activity.changed("vm", now + MAX_WAIT + QUIET);
        assert_eq!(activity.next(now + MAX_WAIT + QUIET), None);
        activity.finished("vm");
        assert_eq!(activity.next(now + MAX_WAIT + QUIET), Some(QUIET));
    }
}
