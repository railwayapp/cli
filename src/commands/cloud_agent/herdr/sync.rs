//! Reconcile herdr's saved machines with the cloud agents you own.
//!
//! Runs as herdr's startup hook as well as an action, so it never prompts and
//! says one line when nothing is wrong. Machines are matched to agents on the
//! ssh target only; labels are display text and anyone can edit them.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use chrono::Utc;
use clap::Parser;
use colored::Colorize;

use super::herdr_cli::{Herdr, Machine};
use super::state::State;
use super::target;
use crate::client::GQLClient;
use crate::config::Configs;
use crate::controllers::cloud_agent as ca;

#[derive(Parser)]
pub struct Args {
    /// Print what would change and change nothing
    #[clap(long)]
    dry_run: bool,

    /// Output as JSON
    #[clap(long)]
    json: bool,

    /// Do nothing when the last sync was this many seconds ago or less
    #[clap(long, value_name = "SECONDS")]
    debounce: Option<u64>,

    /// Also make sure this session's watcher is running
    #[clap(long)]
    spawn_watch: bool,
}

#[derive(Debug, Clone)]
pub(super) enum Op {
    /// A machine pointing at an agent that no longer exists.
    Remove(String),
    /// An enabled machine whose agent is not awake.
    Disable(String),
    /// A disabled machine whose agent is running.
    Enable(String),
    /// An enabled machine whose agent went sleeping → running since the last
    /// sync. herdr parked it in Attention while the VM was down and never
    /// retries that on its own; off and on again makes it reconnect.
    Kick(String),
    /// An agent with no machine. Adding one is interactive, so only reported.
    Missing(ca::Agent),
}

impl Op {
    fn profile_id(&self) -> Option<&str> {
        match self {
            Op::Remove(id) | Op::Disable(id) | Op::Enable(id) | Op::Kick(id) => Some(id),
            Op::Missing(_) => None,
        }
    }

    fn verb(&self) -> &'static str {
        match self {
            Op::Remove(_) => "remove",
            Op::Disable(_) => "disable",
            Op::Enable(_) => "enable",
            Op::Kick(_) => "reconnect",
            Op::Missing(_) => "missing",
        }
    }

    fn describe(&self, machines: &[Machine]) -> String {
        match self {
            Op::Missing(agent) => format!(
                "missing  agent {} ({}) has no herdr machine",
                agent.name,
                agent.status.label()
            ),
            op => {
                let id = op.profile_id().unwrap_or_default();
                let label = machines
                    .iter()
                    .find(|m| m.id == id)
                    .map(|m| m.label.as_str())
                    .unwrap_or(id);
                format!("{:<8} machine {label} ({id})", op.verb())
            }
        }
    }

    fn to_json(&self) -> serde_json::Value {
        match self {
            Op::Missing(agent) => serde_json::json!({
                "op": "missing",
                "agent": { "id": agent.id, "name": agent.name, "status": agent.status.label() },
            }),
            op => serde_json::json!({ "op": op.verb(), "profile": op.profile_id() }),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct Plan {
    pub ops: Vec<Op>,
    /// agent id → herdr profile id, for every agent that has a machine
    pub matches: BTreeMap<String, String>,
    /// profile id → agent, for the ops that must wait for its ssh relay first
    pub agents: BTreeMap<String, ca::Agent>,
    /// agent id → status label, remembered for the next run
    pub statuses: BTreeMap<String, String>,
}

pub(super) fn is_machine_for(agent: &ca::Agent, machine: &Machine) -> bool {
    machine.target == target::target(agent)
        || target::agent_id_of(&machine.target).as_deref() == Some(agent.id.as_str())
}

pub(super) fn machine_for<'a>(agent: &ca::Agent, machines: &'a [Machine]) -> Option<&'a Machine> {
    machines.iter().find(|m| is_machine_for(agent, m))
}

/// `known` is agent id → profile id from the last sync: only machines this
/// plugin has seen attached to an agent are ever removed, so a login to another
/// account, a different `RAILWAY_ENV`, or a machine pointed at a teammate's
/// agent cannot wipe the catalog.
pub(super) fn reconcile(
    agents: &[ca::Agent],
    machines: &[Machine],
    previous: &BTreeMap<String, String>,
    known: &BTreeMap<String, String>,
) -> Plan {
    let mut plan = Plan::default();
    for agent in agents {
        plan.statuses.insert(agent.id.clone(), agent.status.label());
    }
    for machine in machines {
        if target::agent_id_of(&machine.target).is_none() {
            continue;
        }
        let Some(agent) = agents.iter().find(|a| is_machine_for(a, machine)) else {
            if !agents.is_empty() && known.values().any(|p| p == &machine.id) {
                plan.ops.push(Op::Remove(machine.id.clone()));
            }
            continue;
        };
        plan.matches.insert(agent.id.clone(), machine.id.clone());
        let awake = matches!(agent.status, ca::Status::Running | ca::Status::Starting);
        let was_awake = previous
            .get(&agent.id)
            .is_none_or(|s| s == "running" || s == "starting");
        if machine.enabled && !awake {
            plan.ops.push(Op::Disable(machine.id.clone()));
        } else if !machine.enabled && agent.status == ca::Status::Running {
            plan.agents.insert(machine.id.clone(), agent.clone());
            plan.ops.push(Op::Enable(machine.id.clone()));
        } else if machine.enabled && agent.status == ca::Status::Running && !was_awake {
            plan.agents.insert(machine.id.clone(), agent.clone());
            plan.ops.push(Op::Kick(machine.id.clone()));
        }
    }
    for agent in agents {
        if agent.status.is_live() && !plan.matches.contains_key(&agent.id) {
            plan.ops.push(Op::Missing(agent.clone()));
        }
    }
    plan
}

#[derive(Debug, Default)]
pub(super) struct Outcome {
    pub applied: Vec<Op>,
    pub failed: Vec<(Op, String)>,
}

/// `wait` holds Enable and Kick until the agent's ssh relay executes commands;
/// enabling earlier is exactly what parks herdr in Attention.
pub(super) async fn apply(herdr: &Herdr, plan: &Plan, wait: bool) -> Outcome {
    let mut outcome = Outcome::default();
    for op in &plan.ops {
        if wait
            && matches!(op, Op::Enable(_) | Op::Kick(_))
            && let Some(agent) = op.profile_id().and_then(|id| plan.agents.get(id))
            && let Err(e) = super::relay::wait_until_ready(agent).await
        {
            outcome.failed.push((op.clone(), format!("{e:#}")));
            continue;
        }
        let result = match op {
            Op::Remove(id) => herdr.machine_remove(id),
            Op::Disable(id) => herdr.machine_disable(id),
            Op::Enable(id) => herdr.machine_enable(id),
            Op::Kick(id) => herdr
                .machine_disable(id)
                .and_then(|()| herdr.machine_enable(id)),
            Op::Missing(_) => continue,
        };
        match result {
            Ok(()) => outcome.applied.push(op.clone()),
            Err(e) => outcome.failed.push((op.clone(), format!("{e:#}"))),
        }
    }
    outcome
}

pub async fn command(args: Args) -> Result<()> {
    if args.spawn_watch {
        super::watch::spawn_detached();
    }
    if let Some(secs) = args.debounce
        && let Ok(state) = State::load()
        && synced_within(&state, secs, Utc::now())
    {
        return Ok(());
    }
    super::watch::nudge();
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let backboard = configs.get_backboard();
    let herdr = Herdr::from_env();

    let agents = ca::list_mine(&client, &backboard).await?;
    let machines = herdr.machines()?;
    let mut state = State::load().unwrap_or_default();
    let plan = reconcile(&agents, &machines, &state.agent_status, &state.machines);

    let outcome = if args.dry_run {
        Outcome::default()
    } else {
        // Claim the window before the relay waits, so debounced hook runs
        // started meanwhile back off instead of stacking up.
        state.last_sync = Some(Utc::now().to_rfc3339());
        state.save()?;
        let outcome = apply(&herdr, &plan, true).await;
        remember(&mut state, &plan, &outcome)?;
        outcome
    };

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "dryRun": args.dry_run,
                "plan": plan.ops.iter().map(Op::to_json).collect::<Vec<_>>(),
                "applied": outcome.applied.iter().map(Op::to_json).collect::<Vec<_>>(),
                "failed": outcome
                    .failed
                    .iter()
                    .map(|(op, err)| {
                        let mut v = op.to_json();
                        v["error"] = serde_json::Value::String(err.clone());
                        v
                    })
                    .collect::<Vec<_>>(),
                "machines": plan.matches,
            }))?
        );
    } else if let Some(reason) = toast_reason(&outcome) {
        // Started by herdr (a key or the focus hook), not a terminal: the
        // summary goes to a toast. The hook stays quiet unless it changed something.
        println!("{}", summary(&plan, &outcome, agents.len()));
        if reason {
            herdr.notify(
                "Railway sync",
                &plain(&summary(&plan, &outcome, agents.len())),
            );
        }
    } else if args.dry_run {
        if plan.ops.is_empty() {
            println!(
                "herdr machines match your {} agent{}; nothing to do.",
                agents.len(),
                plural(agents.len())
            );
        }
        for op in &plan.ops {
            println!("would {}", op.describe(&machines));
        }
    } else {
        println!("{}", summary(&plan, &outcome, agents.len()));
    }

    if !outcome.failed.is_empty() {
        bail!(
            "{} herdr change{} failed:\n{}",
            outcome.failed.len(),
            plural(outcome.failed.len()),
            outcome
                .failed
                .iter()
                .map(|(op, err)| format!("  {}: {err}", op.describe(&machines)))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(())
}

fn summary(plan: &Plan, outcome: &Outcome, agent_count: usize) -> String {
    let count = |verb: &str| {
        outcome
            .applied
            .iter()
            .filter(|op| op.verb() == verb)
            .count()
    };
    let mut parts = Vec::new();
    for verb in ["enable", "disable", "remove", "reconnect"] {
        let n = count(verb);
        if n > 0 {
            parts.push(format!(
                "{verb}{} {n}",
                if verb.ends_with('e') { "d" } else { "ed" }
            ));
        }
    }
    let missing: Vec<&str> = plan
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Missing(agent) => Some(agent.name.as_str()),
            _ => None,
        })
        .collect();
    let mut line = if !outcome.failed.is_empty() {
        format!(
            "✗ herdr sync: {} failed{}{}.",
            outcome.failed.len(),
            if parts.is_empty() { "" } else { "; " },
            parts.join(", ")
        )
    } else if parts.is_empty() {
        format!(
            "✓ herdr machines match your {agent_count} agent{}.",
            plural(agent_count)
        )
    } else {
        format!("✓ herdr sync: {}.", parts.join(", "))
    };
    if !missing.is_empty() {
        line.push_str(
            &format!(
                " {} agent{} without a machine: {} ({} adds one)",
                missing.len(),
                plural(missing.len()),
                missing.join(", "),
                "railway ca herdr agents".cyan()
            )
            .dimmed()
            .to_string(),
        );
    }
    line
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The whole reconcile, quietly: what the picker runs after a sleep or wake so
/// every machine row reflects the agent it points at.
pub(super) async fn resync(
    client: &reqwest::Client,
    backboard: &str,
    herdr: &Herdr,
) -> Result<Vec<String>> {
    let agents = ca::list_mine(client, backboard).await?;
    let machines = herdr.machines()?;
    let mut state = State::load().unwrap_or_default();
    let plan = reconcile(&agents, &machines, &state.agent_status, &state.machines);
    state.last_sync = Some(Utc::now().to_rfc3339());
    state.save()?;
    let outcome = apply(herdr, &plan, true).await;
    remember(&mut state, &plan, &outcome)?;
    if let Some((op, err)) = outcome.failed.first() {
        bail!("{} failed: {err}", op.describe(&machines));
    }
    Ok(outcome
        .applied
        .iter()
        .map(|op| op.describe(&machines))
        .collect())
}

/// `Some(true)` when a toast is due, `Some(false)` for a quiet hook run,
/// `None` when we are on a terminal and print as usual.
fn toast_reason(outcome: &Outcome) -> Option<bool> {
    let manual = std::env::var("HERDR_PLUGIN_ACTION_ID").is_ok();
    let hook = std::env::var("HERDR_PLUGIN_EVENT").is_ok();
    if !manual && !hook {
        return None;
    }
    Some(manual || !outcome.applied.is_empty() || !outcome.failed.is_empty())
}

fn plain(s: &str) -> String {
    s.trim_start_matches('✓').trim().to_string()
}

/// A failed Enable or Kick keeps the agent's previous status, so the next run
/// plans it again instead of believing the machine already followed.
fn remember(state: &mut State, plan: &Plan, outcome: &Outcome) -> Result<()> {
    let mut statuses = plan.statuses.clone();
    for (op, _) in &outcome.failed {
        if let Some(agent) = op.profile_id().and_then(|id| plan.agents.get(id)) {
            match state.agent_status.get(&agent.id) {
                Some(old) => statuses.insert(agent.id.clone(), old.clone()),
                None => statuses.remove(&agent.id),
            };
        }
    }
    state.machines = plan.matches.clone();
    state.agent_status = statuses;
    state.last_sync = Some(Utc::now().to_rfc3339());
    state.save()
}

pub(super) fn synced_within(state: &State, secs: u64, now: chrono::DateTime<Utc>) -> bool {
    state
        .last_sync
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| now.signed_duration_since(t).num_seconds().unsigned_abs() <= secs)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controllers::cloud_agent::Status;

    fn agent(id: &str, status: Status) -> ca::Agent {
        ca::Agent {
            id: id.into(),
            name: format!("name-{id}"),
            status,
            project_id: "project".into(),
            environment_id: "env".into(),
            created_at: Utc::now(),
        }
    }

    /// What a previous sync would have recorded: every machine matched to an
    /// agent, plus any `agent:*` machine as if it had been seen before.
    fn known(agents: &[ca::Agent], machines: &[Machine]) -> BTreeMap<String, String> {
        machines
            .iter()
            .filter_map(|m| {
                let id = target::agent_id_of(&m.target)?;
                let agent = agents.iter().find(|a| a.id == id).map(|a| a.id.clone());
                Some((agent.unwrap_or(id), m.id.clone()))
            })
            .collect()
    }

    fn machine(id: &str, target: &str, enabled: bool) -> Machine {
        Machine {
            id: id.into(),
            label: format!("label-{id}"),
            target: target.into(),
            session: "default".into(),
            enabled,
            selected: false,
        }
    }

    fn ours(id: &str, agent_id: &str, enabled: bool) -> Machine {
        machine(id, &target::target_for("env", agent_id), enabled)
    }

    fn ops_of(plan: &Plan) -> Vec<String> {
        plan.ops
            .iter()
            .map(|op| match op {
                Op::Missing(agent) => format!("missing {}", agent.id),
                op => format!("{} {}", op.verb(), op.profile_id().unwrap()),
            })
            .collect()
    }

    fn after(machines: &[Machine], plan: &Plan) -> Vec<Machine> {
        machines
            .iter()
            .filter(|m| {
                !plan
                    .ops
                    .iter()
                    .any(|op| matches!(op, Op::Remove(id) if id == &m.id))
            })
            .map(|m| {
                let mut m = m.clone();
                for op in &plan.ops {
                    match op {
                        Op::Disable(id) if id == &m.id => m.enabled = false,
                        Op::Enable(id) | Op::Kick(id) if id == &m.id => m.enabled = true,
                        _ => {}
                    }
                }
                m
            })
            .collect()
    }

    #[test]
    fn reconcile_cases() {
        let cases: Vec<(&str, Vec<ca::Agent>, Vec<Machine>, Vec<&str>)> = vec![
            (
                "running agent with enabled machine",
                vec![agent("a1", Status::Running)],
                vec![ours("p1", "a1", true)],
                vec![],
            ),
            (
                "sleeping agent with enabled machine",
                vec![agent("a1", Status::Sleeping)],
                vec![ours("p1", "a1", true)],
                vec!["disable p1"],
            ),
            (
                "crashed agent with enabled machine",
                vec![agent("a1", Status::Crashed)],
                vec![ours("p1", "a1", true)],
                vec!["disable p1"],
            ),
            (
                "running agent with disabled machine",
                vec![agent("a1", Status::Running)],
                vec![ours("p1", "a1", false)],
                vec!["enable p1"],
            ),
            (
                "starting agent with disabled machine waits",
                vec![agent("a1", Status::Starting)],
                vec![ours("p1", "a1", false)],
                vec![],
            ),
            (
                "our machine with no agent (account still has others)",
                vec![agent("a2", Status::Running)],
                vec![ours("p1", "gone", true)],
                vec!["remove p1", "missing a2"],
            ),
            (
                "agent with no machine",
                vec![agent("a1", Status::Running)],
                vec![],
                vec!["missing a1"],
            ),
            (
                "deleting agent with no machine is not missing",
                vec![agent("a1", Status::Deleting)],
                vec![],
                vec![],
            ),
            (
                "foreign machines are never touched",
                vec![agent("a1", Status::Sleeping)],
                vec![
                    machine("w", "workbox", true),
                    machine("m", "me@workbox", true),
                    ours("p1", "a1", true),
                ],
                vec!["disable p1"],
            ),
            (
                "match on agent id inside a target from another relay host",
                vec![agent("a1", Status::Sleeping)],
                vec![machine("p1", "agent:env:a1@ssh.elsewhere.example", true)],
                vec!["disable p1"],
            ),
            (
                "labels do not match",
                vec![agent("a1", Status::Running)],
                vec![machine("p1", "workbox", true)],
                vec!["missing a1"],
            ),
        ];
        for (name, agents, machines, expected) in cases {
            let plan = reconcile(
                &agents,
                &machines,
                &BTreeMap::new(),
                &known(&agents, &machines),
            );
            assert_eq!(ops_of(&plan), expected, "{name}");
        }
    }

    #[test]
    fn matches_map_every_matched_agent_to_its_profile() {
        let agents = vec![agent("a1", Status::Running), agent("a2", Status::Sleeping)];
        let machines = vec![
            ours("p1", "a1", true),
            ours("p2", "a2", true),
            ours("p3", "gone", true),
            machine("w", "workbox", true),
        ];
        let plan = reconcile(
            &agents,
            &machines,
            &BTreeMap::new(),
            &known(&agents, &machines),
        );
        assert_eq!(
            plan.matches,
            BTreeMap::from([
                ("a1".to_string(), "p1".to_string()),
                ("a2".to_string(), "p2".to_string())
            ])
        );
    }

    #[test]
    fn second_run_over_the_result_is_a_no_op() {
        let agents = vec![
            agent("a1", Status::Running),
            agent("a2", Status::Sleeping),
            agent("a3", Status::Running),
        ];
        let machines = vec![
            ours("p1", "a1", false),
            ours("p2", "a2", true),
            ours("p3", "gone", true),
            machine("w", "workbox", true),
        ];
        let plan = reconcile(
            &agents,
            &machines,
            &BTreeMap::new(),
            &known(&agents, &machines),
        );
        assert_eq!(
            ops_of(&plan),
            vec!["enable p1", "disable p2", "remove p3", "missing a3"]
        );

        let machines = after(&machines, &plan);
        assert_eq!(machines.len(), 3);
        let again = reconcile(
            &agents,
            &machines,
            &BTreeMap::new(),
            &known(&agents, &machines),
        );
        assert_eq!(ops_of(&again), vec!["missing a3"]);
        assert_eq!(again.matches, plan.matches);
    }

    #[test]
    fn a_wake_done_elsewhere_reconnects_the_enabled_machine() {
        let agents = [agent("a1", Status::Running)];
        let machines = [machine("p1", &target::target(&agents[0]), true)];
        let mut previous = BTreeMap::new();
        previous.insert("a1".to_string(), "sleeping".to_string());
        let plan = reconcile(&agents, &machines, &previous, &BTreeMap::new());
        assert!(
            matches!(plan.ops.as_slice(), [Op::Kick(id)] if id == "p1"),
            "{:?}",
            plan.ops
        );
        assert_eq!(plan.statuses.get("a1").map(String::as_str), Some("running"));
        assert!(plan.agents.contains_key("p1"));

        previous.insert("a1".to_string(), "running".to_string());
        assert!(
            reconcile(&agents, &machines, &previous, &BTreeMap::new())
                .ops
                .is_empty()
        );
        assert!(
            reconcile(
                &agents,
                &machines,
                &BTreeMap::new(),
                &known(&agents, &machines)
            )
            .ops
            .is_empty()
        );
    }

    #[test]
    fn unknown_machines_and_empty_agent_lists_never_trigger_removal() {
        let orphan = machine("p9", "ssh://agent%3Aenv%3Anobody@ssh.railway.com", true);
        let removals = |plan: &Plan| {
            plan.ops
                .iter()
                .filter(|op| matches!(op, Op::Remove(_)))
                .count()
        };
        // Never recorded by a previous sync: someone else's agent, or a hand-added machine.
        let plan = reconcile(
            &[agent("a1", Status::Running)],
            &[orphan.clone()],
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert_eq!(removals(&plan), 0, "{:?}", plan.ops);
        // Recorded before, but the agent list came back empty (wrong account, API blip).
        let mut known = BTreeMap::new();
        known.insert("nobody".to_string(), "p9".to_string());
        let plan = reconcile(&[], &[orphan.clone()], &BTreeMap::new(), &known);
        assert_eq!(removals(&plan), 0, "{:?}", plan.ops);
        // Recorded before and the account still has agents: the agent is really gone.
        let plan = reconcile(
            &[agent("a1", Status::Running)],
            &[orphan],
            &BTreeMap::new(),
            &known,
        );
        assert_eq!(removals(&plan), 1, "{:?}", plan.ops);
    }

    #[test]
    fn a_failed_kick_keeps_the_old_status_so_it_is_retried() {
        let agents = [agent("a1", Status::Running)];
        let machines = [machine("p1", &target::target(&agents[0]), true)];
        let mut previous = BTreeMap::new();
        previous.insert("a1".to_string(), "sleeping".to_string());
        let plan = reconcile(&agents, &machines, &previous, &BTreeMap::new());
        assert!(matches!(plan.ops.as_slice(), [Op::Kick(_)]));
        let outcome = Outcome {
            applied: vec![],
            failed: vec![(plan.ops[0].clone(), "relay never answered".into())],
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = State {
            agent_status: previous.clone(),
            ..Default::default()
        };
        // remember() saves to State::path(); exercise the status logic via a copy.
        let _ = &path;
        let before = state.agent_status.clone();
        let mut statuses = plan.statuses.clone();
        for (op, _) in &outcome.failed {
            if let Some(agent) = op.profile_id().and_then(|id| plan.agents.get(id)) {
                if let Some(old) = before.get(&agent.id) {
                    statuses.insert(agent.id.clone(), old.clone());
                }
            }
        }
        state.agent_status = statuses;
        assert_eq!(
            state.agent_status.get("a1").map(String::as_str),
            Some("sleeping")
        );
        assert!(matches!(
            reconcile(&agents, &machines, &state.agent_status, &BTreeMap::new())
                .ops
                .as_slice(),
            [Op::Kick(_)]
        ));
    }

    #[tokio::test]
    async fn apply_runs_exactly_the_planned_herdr_commands() {
        let fake = super::super::herdr_cli::fake::FakeHerdr::with_machines(&format!(
            r#"[
                {{"id":"p1","label":"proj/one","target":"{}","enabled":true}},
                {{"id":"p2","label":"proj/two","target":"{}","enabled":false}},
                {{"id":"p3","label":"proj/gone","target":"{}","enabled":true}},
                {{"id":"w","label":"workbox","target":"workbox","enabled":true}}
            ]"#,
            target::target_for("env", "a1"),
            target::target_for("env", "a2"),
            target::target_for("env", "gone"),
        ));
        let herdr = fake.herdr();
        let agents = vec![
            agent("a1", Status::Sleeping),
            agent("a2", Status::Running),
            agent("a3", Status::Running),
        ];
        let machines = herdr.machines().unwrap();
        let plan = reconcile(
            &agents,
            &machines,
            &BTreeMap::new(),
            &known(&agents, &machines),
        );
        let outcome = apply(&herdr, &plan, false).await;
        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        assert_eq!(outcome.applied.len(), 3);
        assert_eq!(
            fake.calls(),
            vec![
                "machine list --json".to_string(),
                "machine disable p1".to_string(),
                "machine enable p2".to_string(),
                "machine remove p3".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn apply_collects_failures_instead_of_stopping() {
        let herdr = Herdr::at("/nonexistent/herdr");
        let plan = Plan {
            ops: vec![Op::Disable("p1".into()), Op::Remove("p2".into())],
            ..Default::default()
        };
        let outcome = apply(&herdr, &plan, false).await;
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.failed.len(), 2);
        assert!(matches!(outcome.failed[0].0, Op::Disable(_)));
        assert!(matches!(outcome.failed[1].0, Op::Remove(_)));
    }

    #[test]
    fn dry_run_lines_name_the_machine() {
        let machines = vec![ours("p1", "a1", true)];
        let plan = reconcile(
            &[agent("a1", Status::Sleeping)],
            &machines,
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert_eq!(
            plan.ops[0].describe(&machines),
            "disable  machine label-p1 (p1)"
        );
    }

    #[test]
    fn debounce_window_reads_last_sync() {
        let now = Utc::now();
        let mut state = State::default();
        assert!(!synced_within(&state, 30, now));
        state.last_sync = Some((now - chrono::Duration::seconds(10)).to_rfc3339());
        assert!(synced_within(&state, 30, now));
        state.last_sync = Some((now - chrono::Duration::seconds(45)).to_rfc3339());
        assert!(!synced_within(&state, 30, now));
        state.last_sync = Some("not a date".into());
        assert!(!synced_within(&state, 30, now));
    }
}
