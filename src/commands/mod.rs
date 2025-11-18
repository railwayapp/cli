pub(super) use crate::{client::*, config::*, gql::*};
pub(super) use anyhow::{Context, Result, bail};
pub(super) use clap::{Parser, Subcommand};
pub(super) use colored::Colorize;

pub fn get_dynamic_args(cmd: clap::Command) -> clap::Command {
    // no-op
    cmd
}

pub mod add;
pub mod completion;
pub mod connect;
pub mod deploy;
pub mod deployment;
pub mod docs;
pub mod domain;
pub mod down;
pub mod environment;
pub mod functions;
pub mod init;
pub mod link;
pub mod list;
pub mod login;
pub mod logout;
pub mod logs;
pub mod open;
pub mod redeploy;
pub mod run;
pub mod scale;
pub mod service;
pub mod shell;
pub mod ssh;
pub mod starship;
pub mod status;
pub mod unlink;
pub mod up;
pub mod variables;
pub mod volume;
pub mod whoami;

pub mod check_updates;
