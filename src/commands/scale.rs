use crate::config;
use clap::{Arg, Command};
use futures::executor::block_on;
use serde::Serialize;

use super::{
    queries::{
        projects::ProjectsProjectsEdgesNode, user_projects::UserProjectsMeProjectsEdgesNode,
    },
    *,
};

/// List all projects in your Railway account
#[derive(Parser, Debug)]
pub struct Args {}

pub fn get_dynamic_args(cmd: Command) -> Command {
    if !std::env::args().any(|f| f.eq_ignore_ascii_case("scale")) {
        // if the command has nothing to do with railway scale, dont make the web request.
        return cmd;
    }
    block_on(async move {
        let configs = Configs::new().unwrap();
        let client = GQLClient::new_authorized(&configs).unwrap();
        let regions = post_graphql::<queries::Regions, _>(
            &client,
            configs.get_backboard(),
            queries::regions::Variables,
        )
        .await
        .expect("couldn't get regions");

        // Collect region names as owned Strings.
        let region_strings = regions
            .regions
            .iter()
            .map(|r| r.name.to_string())
            .collect::<Vec<String>>();

        // Mutate the command to add each region as a flag.
        let mut new_cmd = cmd;
        for region in region_strings {
            let region_static: &'static str = Box::leak(region.into_boxed_str());
            new_cmd = new_cmd.arg(
                Arg::new(region_static) // unique identifier
                    .long(region_static)        // --my-region
                    .help(format!("Number of instances to run on {}", region_static))
                    .value_name("INSTANCES")
                    .value_parser(clap::value_parser!(u16))
                    .action(clap::ArgAction::Set)
            );
        }
        new_cmd
    })
}

pub async fn command(args: Args, json: bool) -> Result<()> {
    // Args::command().;
    println!("hello");
    println!("{:?}", args);
    Ok(())
}
