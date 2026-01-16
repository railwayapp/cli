use anyhow::Result;
use strum::{Display, EnumIter};

pub use crate::controllers::config::PatchEntry;

pub mod builder;
pub mod healthcheck;
pub mod regions;
pub mod restart_policy;
pub mod source;
pub mod variable;

#[derive(Clone, Copy, Display, EnumIter)]
#[strum(serialize_all = "title_case")]
pub enum Change {
    Variables,
    Sources,
    Builder,
    Healthcheck,
    Regions,
    RestartPolicy,
}

macro_rules! register_handlers {
    ($($variant:ident => $module:ident),* $(,)?) => {
        impl Change {
            pub fn parse_interactive(&self, service_id: &str, service_name: &str) -> Result<Vec<PatchEntry>> {
                match self {
                    $(Change::$variant => $module::parse_interactive(service_id, service_name),)*
                }
            }
        }
    };
}

register_handlers!(
    Variables => variable,
    Sources => source,
    Builder => builder,
    Healthcheck => healthcheck,
    Regions => regions,
    RestartPolicy => restart_policy,
);
