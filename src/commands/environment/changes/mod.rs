use crate::commands::environment::EnvironmentConfigOptions;
use anyhow::Result;
use derive_more::Display as DeriveDisplay;
use strum::{Display, EnumDiscriminants, EnumIter, VariantNames};

pub mod restart_policy;
pub mod source;
pub mod variable;

use restart_policy::RestartPolicy;
use source::Source;
use variable::Variable;

#[derive(Clone, DeriveDisplay, EnumDiscriminants)]
#[strum_discriminants(derive(Display, EnumIter, VariantNames), name(ChangeOption))]
/// In order to add a new Change that can be configured, add a variant to this enum and give it it's own type
/// Implement the ChangeHandler trait for the type
/// Add that type to the invocation of the register_handlers! macro
pub enum Change {
    #[strum_discriminants(strum(serialize = "Variables"))]
    Variable(variable::Variable),
    #[strum_discriminants(strum(serialize = "Sources"))]
    Source(source::Source),
    #[strum_discriminants(strum(serialize = "Restart policy"))]
    RestartPolicy(restart_policy::RestartPolicy),
}

impl Change {
    pub fn variant_name(&self) -> String {
        ChangeOption::from(self)
            .to_string()
            .trim_end_matches('s')
            .to_lowercase()
    }
}

/// Trait for handling the parsing of a change
trait ChangeHandler: Clone {
    /// Get the command-line args for this change type (if fixed amount of values, should be chunked)
    fn get_args(args: &EnvironmentConfigOptions) -> Vec<Vec<String>>;

    /// Parse from non-interactive arguments
    fn parse_non_interactive(args: Vec<Vec<String>>) -> Vec<(String, Self)>;

    /// Parse interactively for a specific service
    fn parse_interactive(service_name: &str) -> Result<Vec<Self>>;

    /// Convert to Change enum
    fn into_change(self) -> Change;
}

macro_rules! register_handlers {
    ($($type:ident),* $(,)?) => {
        impl ChangeOption {
            pub fn get_args(&self, args: &EnvironmentConfigOptions) -> Vec<Vec<String>> {
                match self {
                    $(ChangeOption::$type => <$type>::get_args(args),)*
                }
            }

            pub fn parse_non_interactive(&self, args: Vec<Vec<String>>) -> Vec<(String, Change)> {
                if args.is_empty() || args.iter().all(|v| v.is_empty()) {
                    return Vec::new();
                }

                match self {
                    $(
                        ChangeOption::$type => {
                            <$type>::parse_non_interactive(args)
                                .into_iter()
                                .map(|(s, item)| (s, item.into_change()))
                                .collect()
                        }
                    )*
                }
            }

            pub fn parse_interactive(&self, service_name: &str) -> Result<Vec<Change>> {
                match self {
                    $(
                        ChangeOption::$type => {
                            Ok(<$type>::parse_interactive(service_name)?
                                .into_iter()
                                .map(|item| item.into_change())
                                .collect())
                        }
                    )*
                }
            }
        }
    };
}

pub fn chunk(v: &[String], chunks: usize) -> Vec<Vec<String>> {
    v.chunks(chunks).map(|c| c.to_vec()).collect()
}

// Register all handlers (generates helper functions)
register_handlers!(Variable, Source, RestartPolicy);
