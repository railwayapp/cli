use anyhow::Result;
use strum::{Display, EnumIter};

pub use crate::controllers::config::{PatchEntry, ServiceInstance};

pub mod build_command;
pub mod builder;
pub mod healthcheck;
pub mod regions;
pub mod restart_policy;
pub mod source;
pub mod start_command;
pub mod variable;
pub mod watch_patterns;

#[derive(Clone, Copy, Display, EnumIter)]
#[strum(serialize_all = "title_case")]
pub enum Change {
    Variables,
    Sources,
    Builder,
    BuildCommand,
    StartCommand,
    Healthcheck,
    Regions,
    RestartPolicy,
    WatchPatterns,
}

macro_rules! register_handlers {
    ($($variant:ident => $module:ident),* $(,)?) => {
        impl Change {
            pub fn parse_interactive(
                &self,
                service_id: &str,
                service_name: &str,
                existing: Option<&ServiceInstance>,
            ) -> Result<Vec<PatchEntry>> {
                match self {
                    $(Change::$variant => $module::parse_interactive(service_id, service_name, existing),)*
                }
            }
        }
    };
}

register_handlers!(
    Variables => variable,
    Sources => source,
    Builder => builder,
    BuildCommand => build_command,
    StartCommand => start_command,
    Healthcheck => healthcheck,
    Regions => regions,
    RestartPolicy => restart_policy,
    WatchPatterns => watch_patterns,
);
