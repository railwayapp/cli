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
use super::state::State;
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
    state: State,
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
    let state = State::load().unwrap_or_default();
    let mut picker = Picker {
        configs,
        client,
        backboard,
        herdr,
        state,
        remote: args.remote,
    };
    let this_vm = std::env::var("RAILWAY_CLOUD_AGENT_ID").ok();
    let mut names = picker.state.project_names.clone();

    loop {
        let agents = ca::list_mine(&picker.client, &picker.backboard).await?;
        if agents.iter().any(|a| !names.contains_key(&a.project_id)) {
            names = lifecycle::place_names(&picker.client, &picker.configs)
                .await
                .into_iter()
                .collect();
            if !names.is_empty() {
                picker.state.project_names = names.clone();
                let _ = picker.state.save();
            }
        }
        if agents.is_empty() {
            println!(
                "No cloud agents. {} creates one and adds it to herdr.",
                "railway ca herdr new".cyan()
            );
            return Ok(());
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
                    picker.resync().await
                }
                _ => match inquire::Select::new("Wake", rows)
                    .with_render_config(Configs::get_render_config())
                    .with_page_size(15)
                    .with_help_message("↑↓ move, type to filter, enter wakes, esc quits")
                    .prompt_skippable()?
                {
                    Some(row) => {
                        picker.wake(&row).await?;
                        picker.resync().await
                    }
                    None => Ok(()),
                },
            };
        }

        let Some(row) = inquire::Select::new("Agent", rows)
            .with_render_config(Configs::get_render_config())
            .with_page_size(15)
            .with_help_message("↑↓ move, type to filter, enter picks, esc quits")
            .prompt_skippable()?
        else {
            return Ok(());
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
            Action::Sleep => picker.sleep(&row).await.and(picker.resync().await),
            Action::Wake => {
                picker.wake(&row).await?;
                picker.resync().await?;
                return Ok(());
            }
            Action::Delete => picker.delete(&row).await,
            Action::New => return super::new::command(super::new::Args::interactive()).await,
            Action::Quit => return Ok(()),
        };
        if let Err(e) = result {
            eprintln!("{} {e:#}", "✗".red());
        }
    }
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
                self.herdr.machine_disable(&machine.id)?;
                self.herdr.machine_enable(&machine.id)?;
                self.remember(&agent.id, Some(&machine.id))?;
                println!(
                    "✓ {} is enabled in herdr; pick it from the sidebar.",
                    machine.label.cyan()
                );
            }
            None => {
                let target = target::target(&agent);
                let label = target::label(&row.project, &agent.name);
                super::known_hosts::ensure_relay_known_host()?;
                self.herdr.machine_add(&target, &label)?;
                let added = self
                    .herdr
                    .machines()?
                    .into_iter()
                    .find(|m| sync::is_machine_for(&agent, m));
                self.remember(&agent.id, added.as_ref().map(|m| m.id.as_str()))?;
                let harness = super::harness::choose(&Default::default(), true)?;
                if let Err(e) = super::bootstrap::run(&agent, harness).await {
                    eprintln!(
                        "{} bootstrap did not finish: {e:#}\n  {} retries it.",
                        "warning:".yellow(),
                        format!("railway ca herdr bootstrap {}", agent.name).cyan()
                    );
                }
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
        match agent.status {
            ca::Status::Sleeping => {
                println!("Agent {} is already asleep.", agent.name.cyan());
            }
            ca::Status::Running | ca::Status::Starting => {
                self.herdr.notify(
                    &format!("Sleeping {}", agent.name),
                    "railway: cloudAgentSleep issued; its herdr machine is being disabled",
                );
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
                println!(
                    "✓ Sleeping agent {}; its disk is kept, compute stops billing.",
                    agent.name.cyan()
                );
            }
            _ => bail!(
                "Agent {} is {}; there is nothing running to sleep.",
                agent.name,
                agent.status.label()
            ),
        }
        if let Some(machine) = &row.machine {
            if machine.enabled {
                self.herdr.machine_disable(&machine.id)?;
            }
            self.remember(&agent.id, Some(&machine.id))?;
        }
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
                self.herdr.machine_disable(&machine.id)?;
                self.herdr.machine_enable(&machine.id)?;
                self.remember(&agent.id, Some(&machine.id))?;
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
        self.remember(&agent.id, None)?;
        println!("✓ Deleted agent {}", agent.name.cyan());
        Ok(())
    }

    async fn ensure_awake(&mut self, agent: &ca::Agent) -> Result<ca::Agent> {
        match agent.status {
            ca::Status::Running => {
                ca::remember(&mut self.configs, agent)?;
                return Ok(agent.clone());
            }
            ca::Status::Sleeping => {
                self.herdr.notify(
                    &format!("Waking {}", agent.name),
                    "railway: cloudAgentWake issued; the machine is re-enabled once its ssh relay answers",
                );
                ca::wake(&self.client, &self.backboard, &agent.id).await?;
            }
            ca::Status::Starting => {}
            _ => bail!(
                "Agent {} is {} and cannot be woken.",
                agent.name,
                agent.status.label()
            ),
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

    async fn resync(&mut self) -> Result<()> {
        if self.remote {
            return Ok(());
        }
        sync::resync(&self.client, &self.backboard, &self.herdr).await?;
        self.state = State::load().unwrap_or_default();
        Ok(())
    }

    fn remember(&mut self, agent_id: &str, profile_id: Option<&str>) -> Result<()> {
        match profile_id {
            Some(profile) => {
                self.state
                    .machines
                    .insert(agent_id.to_string(), profile.to_string());
            }
            None => {
                self.state.machines.remove(agent_id);
            }
        }
        self.state.save()
    }
}
