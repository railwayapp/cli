use clap::{Arg, Command};
use futures::executor::block_on;
use serde_json::json;
use std::collections::HashMap;

use super::*;
/// Dynamic flags workaround
/// Unfortunately, we aren't able to use the Parser derive macro when working with dynamic flags,
/// meaning we have to implement most of the traits for the Args struct manually.
pub struct Args {
    // This field will collect any of the dynamically generated flags
    pub dynamic: HashMap<String, u16>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    
    Ok(())
}

/// This function generates flags that are appended to the command at runtime.
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

impl clap::FromArgMatches for Args {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        let mut dynamic = HashMap::new();
        // Iterate through all provided argument keys.
        // Adjust the static key names if you add any to your Args struct.
        for key in matches.ids() {
            if key == "json" {
                continue;
            }
            // If the flag value can be interpreted as a u16, insert it.
            if let Some(val) = matches.get_one::<u16>(key.as_str()) {
                dynamic.insert(key.to_string(), *val);
            }
        }
        Ok(Args { dynamic })
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl clap::Args for Args {
    fn group_id() -> Option<clap::Id> {
        Some(clap::Id::from("Args"))
    }
    fn augment_args<'b>(__clap_app: clap::Command) -> clap::Command {
        {
            let __clap_app = __clap_app.group(clap::ArgGroup::new("Args").multiple(true).args({
                let members: [clap::Id; 0usize] = [];
                members
            }));
            __clap_app
                .about("Control the number of instances running in each region")
                .long_about(None)
        }
    }
    fn augment_args_for_update<'b>(__clap_app: clap::Command) -> clap::Command {
        {
            let __clap_app = __clap_app.group(clap::ArgGroup::new("Args").multiple(true).args({
                let members: [clap::Id; 0usize] = [];
                members
            }));
            __clap_app
                .about("Control the number of instances running in each region")
                .long_about(None)
        }
    }
}

impl clap::CommandFactory for Args {
    fn command<'b>() -> clap::Command {
        let __clap_app = clap::Command::new("railwayapp");
        <Args as clap::Args>::augment_args(__clap_app)
    }
    fn command_for_update<'b>() -> clap::Command {
        let __clap_app = clap::Command::new("railwayapp");
        <Args as clap::Args>::augment_args_for_update(__clap_app)
    }
}
