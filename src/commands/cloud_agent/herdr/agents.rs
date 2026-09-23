//! `railway ca herdr agents`: the picker herdr's popup runs.
//!
//! One list of every agent you own with its herdr machine state, then an
//! action on the one you pick. Sleep, wake and delete change the VM through
//! the same controller paths as `railway ca`, then keep the herdr machine in
//! step so the sidebar stops retrying a VM that is deliberately off.

use std::fmt;

use anyhow::{Result, bail};
use clap::Parser;
use colored::Colorize;

use super::herdr_cli::{Herdr, Machine};
use super::state::Store;
use super::sync;
use super::target;
use crate::client::GQLClient;
use crate::commands::cloud_agent::lifecycle;
use crate::config::Configs;
use crate::controllers::cloud_agent as ca;
use crate::util::progress::create_spinner;
use crate::util::prompt::{prompt_confirm_with_default, prompt_select_with_cancel};

#[derive(Parser)]
pub struct Args {
    /// Open the picker in a herdr popup pane instead of this terminal
    #[clap(long)]
    open: bool,

    /// Running on a cloud agent VM: no herdr machines here, so connect and new
    /// are left to Local; the row for this VM is marked
    #[clap(long)]
    remote: bool,

    /// Only sleeping agents; with exactly one, wake it without asking
    #[clap(long)]
    wake: bool,
}

struct Row {
    agent: ca::Agent,
    project: String,
    machine: Option<Machine>,
    remote: bool,
    this_vm: bool,
}

impl Row {
    fn machine_state(&self) -> &'static str {
        match &self.machine {
            Some(m) if m.enabled => "machine",
            Some(_) => "machine (disabled)",
            None => "no machine",
        }
    }
}

impl fmt::Display for Row {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<9}  {}/{}",
            self.agent.status.label(),
            self.project,
            self.agent.name
        )?;
        if self.this_vm {
            write!(f, "  ← this VM")
        } else if self.remote {
            Ok(())
        } else {
            write!(f, "  {}", self.machine_state())
        }
    }
}

/// The list mixes the two things that are not about one agent with the agents.
enum Item {
    New,
    Sync,
    Agent(Row),
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Item::New => f.write_str("+ new agent"),
            Item::Sync => f.write_str("↻ sync now"),
            Item::Agent(row) => row.fmt(f),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Connect,
    Sleep,
    Wake,
    Delete,
    New,
    Quit,
}

impl Action {
    const ALL: [Action; 6] = [
        Action::Connect,
        Action::Sleep,
        Action::Wake,
        Action::Delete,
        Action::New,
        Action::Quit,
    ];

    /// No machine catalog on a VM, so nothing to connect or add there.
    const REMOTE: [Action; 4] = [Action::Sleep, Action::Wake, Action::Delete, Action::Quit];
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Action::Connect => "connect    enable its herdr machine, waking it first if asleep",
            Action::Sleep => "sleep      stop the compute bill, keep the disk",
            Action::Wake => "wake       bring it back and re-enable its machine",
            Action::Delete => "delete     the agent, its disk and its machine",
            Action::New => "new agent  create one and add it to herdr",
            Action::Quit => "quit",
        })
    }
}

struct Picker {
    configs: Configs,
    client: reqwest::Client,
    backboard: String,
    herdr: Herdr,
    store: Store,
    remote: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let herdr = Herdr::from_env();
    if args.open {
        let entrypoint = if args.wake { "wake" } else { "agents" };
        return herdr.plugin_pane_open(super::PLUGIN_ID, entrypoint);
    }

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let backboard = configs.get_backboard();
    let store = Store::new(&configs)?;
    let mut picker = Picker {
        configs,
        client,
        backboard,
        herdr,
        store,
        remote: args.remote,
    };
    let this_vm = std::env::var("RAILWAY_CLOUD_AGENT_ID").ok();
    let mut names = picker.store.load()?.project_names;

    loop {
        let agents = ca::list_mine(&picker.client, &picker.backboard).await?;
        if agents.iter().any(|a| !names.contains_key(&a.project_id)) {
            names = lifecycle::place_names(&picker.client, &picker.configs)
                .await
                .into_iter()
                .collect();
            if !names.is_empty() {
                picker
                    .store
                    .update(|s| s.project_names = names.clone())
                    .await?;
            }
        }
        let machines = if args.remote {
            Vec::new()
        } else {
            picker.herdr.machines()?
        };
        let mut rows: Vec<Row> = agents
            .into_iter()
            .filter(|agent| !args.wake || matches!(agent.status, ca::Status::Sleeping))
            .map(|agent| Row {
                machine: sync::machine_for(&agent, &machines).cloned(),
                project: names
                    .get(&agent.project_id)
                    .cloned()
                    .unwrap_or_else(|| agent.project_id.clone()),
                remote: args.remote,
                this_vm: this_vm.as_deref() == Some(agent.id.as_str()),
                agent,
            })
            .collect();
        rows.sort_by(|a, b| {
            a.project
                .cmp(&b.project)
                .then_with(|| a.agent.name.cmp(&b.agent.name))
        });

        if args.wake {
            return match rows.len() {
                0 => {
                    println!("No sleeping agents.");
                    Ok(())
                }
                1 => {
                    picker.wake(&rows[0]).await?;
                    picker.resync().await.map(drop)
                }
                _ => match inquire::Select::new("Wake", rows)
                    .with_render_config(Configs::get_render_config())
                    .with_page_size(15)
                    .with_help_message("↑↓ move, type to filter, enter wakes, esc quits")
                    .prompt_skippable()?
                {
                    Some(row) => {
                        picker.wake(&row).await?;
                        picker.resync().await.map(drop)
                    }
                    None => Ok(()),
                },
            };
        }

        let items = picker_items(rows, args.remote);
        if items.is_empty() {
            println!(
                "No cloud agents. {} creates one and adds it to herdr.",
                "railway ca herdr new".cyan()
            );
            return Ok(());
        }
        let Some(item) = inquire::Select::new("Agent", items)
            .with_render_config(Configs::get_render_config())
            .with_page_size(17)
            .with_help_message("↑↓ move, type to filter, enter picks, esc quits")
            .prompt_skippable()?
        else {
            return Ok(());
        };
        let row = match item {
            Item::New => return super::new::command(super::new::Args::interactive()).await,
            Item::Sync => {
                match picker.resync().await {
                    Ok(applied) if applied.is_empty() => {
                        println!("✓ herdr machines match your agents.")
                    }
                    Ok(applied) => println!("✓ herdr sync: {}", applied.join(", ")),
                    Err(e) => eprintln!("{} {e:#}", "✗".red()),
                }
                continue;
            }
            Item::Agent(row) => row,
        };
        let actions = if args.remote {
            Action::REMOTE.to_vec()
        } else {
            Action::ALL.to_vec()
        };
        let Some(action) =
            prompt_select_with_cancel(&format!("{}/{}", row.project, row.agent.name), actions)?
        else {
            continue;
        };

        // connect and new leave a machine to look at, so the popup closes
        // behind them; the rest stay on the list.
        let result = match action {
            Action::Connect => return picker.connect(&row).await,
            Action::Sleep => picker
                .sleep(&row)
                .await
                .and(picker.resync().await.map(drop)),
            Action::Wake => match picker.wake(&row).await {
                Ok(()) => {
                    picker.resync().await?;
                    return Ok(());
                }
                Err(e) => Err(e),
            },
            Action::Delete => picker
                .delete(&row)
                .await
                .and(picker.resync().await.map(drop)),
            Action::New => return super::new::command(super::new::Args::interactive()).await,
            Action::Quit => return Ok(()),
        };
        if let Err(e) = result {
            eprintln!("{} {e:#}", "✗".red());
        }
    }
}

fn picker_items(rows: Vec<Row>, remote: bool) -> Vec<Item> {
    if remote && rows.is_empty() {
        return Vec::new();
    }
    let mut items = Vec::with_capacity(rows.len() + 2);
    if !remote {
        items.push(Item::New);
    }
    items.push(Item::Sync);
    items.extend(rows.into_iter().map(Item::Agent));
    items
}

impl Picker {
    async fn connect(&mut self, row: &Row) -> Result<()> {
        let agent = self.ensure_awake(&row.agent).await?;
        let spinner = create_spinner(format!("Waiting for {}'s ssh relay", agent.name));
        let ready = super::relay::wait_until_ready(&agent).await;
        spinner.finish_and_clear();
        ready?;
        match &row.machine {
            Some(machine) => {
                self.reconnect(&agent, machine).await?;
                if let Some(harness) = self.store.load()?.bootstrap_pending.get(&agent.id).cloned()
                {
                    super::bootstrap::run(&agent, &harness, &self.store).await?;
                }
                println!(
                    "✓ {} is enabled in herdr; pick it from the sidebar.",
                    machine.label.cyan()
                );
            }
            None => {
                let harness = super::harness::choose(&Default::default(), true)?;
                let target = target::target(&agent);
                let label = target::label(&row.project, &agent.name);
                super::known_hosts::ensure_relay_known_host()?;
                self.herdr.machine_add(&target, &label)?;
                let added = self
                    .herdr
                    .machines()?
                    .into_iter()
                    .find(|m| sync::is_machine_for(&agent, m));
                self.remember(&agent.id, added.as_ref().map(|m| m.id.as_str()))
                    .await?;
                super::bootstrap::run(&agent, harness, &self.store).await?;
                println!(
                    "✓ Added {} to herdr; pick it from the sidebar.",
                    label.cyan()
                );
            }
        }
        Ok(())
    }

    async fn sleep(&mut self, row: &Row) -> Result<()> {
        let agent = &row.agent;
        self.herdr.notify(
            &format!("Sleeping {}", agent.name),
            "Requesting sleep; its disk is kept",
        );
        let mut locked = self.store.lock().await?;
        let spinner = create_spinner(format!("Sleeping agent {}", agent.name));
        let result = ca::sleep(
            &self.client,
            &self.backboard,
            &agent.environment_id,
            &agent.id,
        )
        .await;
        spinner.finish_and_clear();
        result?;
        locked.state.sleep_until.insert(
            agent.id.clone(),
            chrono::Utc::now() + chrono::Duration::seconds(60),
        );
        // Record the acknowledgement even if disabling Herdr subsequently fails.
        locked.save()?;
        if let Some(machine) = &row.machine {
            self.herdr.machine_disable(&machine.id)?;
        }
        println!(
            "✓ Sleep requested for agent {}; compute stops billing once it is asleep.",
            agent.name.cyan()
        );
        Ok(())
    }

    async fn wake(&mut self, row: &Row) -> Result<()> {
        let agent = self.ensure_awake(&row.agent).await?;
        match &row.machine {
            Some(machine) => {
                let spinner = create_spinner(format!("Waiting for {}'s ssh relay", agent.name));
                let ready = super::relay::wait_until_ready(&agent).await;
                spinner.finish_and_clear();
                ready?;
                // Off then on: a profile change makes herdr open a fresh
                // connection, which is what clears a stuck Attention state.
                self.reconnect(&agent, machine).await?;
                println!(
                    "✓ Agent {} is running; {} is back in the sidebar.",
                    agent.name.cyan(),
                    machine.label.cyan()
                );
            }
            None if self.remote => println!("✓ Agent {} is running.", agent.name.cyan()),
            None => println!(
                "✓ Agent {} is running. It has no herdr machine; {} adds one.",
                agent.name.cyan(),
                "connect".cyan()
            ),
        }
        Ok(())
    }

    async fn delete(&mut self, row: &Row) -> Result<()> {
        let agent = &row.agent;
        let confirmed = prompt_confirm_with_default(
            &format!(
                "Delete agent {} and everything on its disk?",
                agent.name.cyan()
            ),
            false,
        )?;
        if !confirmed {
            println!("Left agent {} alone.", agent.name);
            return Ok(());
        }

        let mut locked = self.store.lock().await?;
        let spinner = create_spinner(format!("Deleting agent {}", agent.name));
        let result = ca::delete(&self.client, &self.backboard, &agent.id).await;
        spinner.finish_and_clear();
        // Same rule as `railway ca delete`: forget the pointer even when the
        // mutation failed, so a gone agent is never reached for again.
        ca::forget(&mut self.configs, &agent.environment_id)?;
        result?;

        if let Some(machine) = &row.machine {
            self.herdr.machine_remove(&machine.id)?;
        }
        locked.state.machines.remove(&agent.id);
        locked.state.sleep_until.remove(&agent.id);
        locked.state.bootstrap_pending.remove(&agent.id);
        locked.save()?;
        println!("✓ Deleted agent {}", agent.name.cyan());
        Ok(())
    }

    async fn ensure_awake(&mut self, agent: &ca::Agent) -> Result<ca::Agent> {
        // Inventory can lag a sleep/wake elsewhere. The server reads live VM
        // state and handles no-ops; always send the user's intent to it.
        self.herdr.notify(
            &format!("Waking {}", agent.name),
            "The machine is re-enabled once its SSH relay answers",
        );
        {
            let mut locked = self.store.lock().await?;
            ca::wake(&self.client, &self.backboard, &agent.id).await?;
            locked.state.sleep_until.remove(&agent.id);
            locked.save()?;
        }
        ca::remember(&mut self.configs, agent)?;
        let spinner = create_spinner(format!("Waking agent {}", agent.name));
        let result = ca::wait_until_running(
            &self.client,
            &self.backboard,
            &agent.environment_id,
            &agent.id,
        )
        .await;
        spinner.finish_and_clear();
        result
    }

    async fn resync(&mut self) -> Result<Vec<String>> {
        if self.remote {
            return Ok(Vec::new());
        }
        sync::resync(&self.client, &self.backboard, &self.herdr, &self.store).await
    }

    async fn reconnect(&self, agent: &ca::Agent, machine: &Machine) -> Result<()> {
        let mut locked = self.store.lock().await?;
        if locked.state.sleep_pending(&agent.id, chrono::Utc::now()) {
            bail!(
                "A sleep was requested for {} while connecting; wake it again to reconnect.",
                agent.name
            );
        }
        self.herdr.machine_disable(&machine.id)?;
        self.herdr.machine_enable(&machine.id)?;
        locked
            .state
            .machines
            .insert(agent.id.clone(), machine.id.clone());
        locked.save()
    }

    async fn remember(&self, agent_id: &str, profile_id: Option<&str>) -> Result<()> {
        self.store
            .update(|state| match profile_id {
                Some(profile) => {
                    state
                        .machines
                        .insert(agent_id.to_string(), profile.to_string());
                }
                None => {
                    state.machines.remove(agent_id);
                }
            })
            .await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::testkit::MockBackboard;

    #[tokio::test]
    async fn an_observed_running_agent_still_sends_the_wake_request() {
        let api = MockBackboard::spawn();
        let home = tempfile::tempdir().unwrap();
        let fake = super::super::herdr_cli::fake::FakeHerdr::with_machines("[]");
        let agent = ca::Agent {
            id: "agent-1".into(),
            name: "reviewer".into(),
            status: ca::Status::Running,
            project_id: "project-1".into(),
            environment_id: "environment-1".into(),
            created_at: chrono::Utc::now(),
        };
        api.stub(
            "CloudAgentWake",
            serde_json::json!({"cloudAgentWake": {"id": agent.id, "status": "STARTING"}}),
        );
        api.stub(
            "CloudAgent",
            serde_json::json!({"cloudAgent": {
                "id": agent.id, "name": agent.name, "status": "RUNNING",
                "projectId": agent.project_id, "environmentId": agent.environment_id,
                "createdAt": agent.created_at,
            }}),
        );
        let mut picker = Picker {
            configs: api.configs(&home),
            client: reqwest::Client::new(),
            backboard: api.url(),
            herdr: fake.herdr(),
            store: Store::at(home.path().join("state.json"), &api.url(), "test"),
            remote: false,
        };
        picker.ensure_awake(&agent).await.unwrap();
        assert!(
            api.requests()
                .iter()
                .any(|r| r["operationName"] == "CloudAgentWake"),
            "an inventory observation must not veto explicit wake intent"
        );
    }

    #[test]
    fn empty_local_picker_offers_creation() {
        let items = picker_items(Vec::new(), false);
        assert!(matches!(items.first(), Some(Item::New)));
        assert!(picker_items(Vec::new(), true).is_empty());
    }
}
