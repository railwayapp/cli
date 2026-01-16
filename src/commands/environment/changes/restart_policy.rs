use std::str::FromStr;

use super::*;
use crate::util::prompt::{prompt_options, prompt_text};
use strum::{EnumDiscriminants, EnumString, IntoEnumIterator};

#[derive(Clone, Debug, DeriveDisplay, EnumDiscriminants)]
#[strum_discriminants(
    derive(Display, EnumIter, EnumString),
    name(RestartPolicyTypes),
    strum(ascii_case_insensitive)
)]
pub enum RestartPolicy {
    Never,
    Always,
    #[strum_discriminants(strum(
        serialize = "on_failure",
        serialize = "on failure",
        serialize = "failure",
        to_string = "on_failure"
    ))]
    #[display("On failure: {} max attempts", _0)]
    OnFailure(u16),
}

impl ChangeHandler for RestartPolicy {
    fn get_args(args: &EnvironmentConfigOptions) -> Vec<Vec<String>> {
        args.service_restart_policies
            .iter()
            .map(|g| g.0.to_vec())
            .collect()
    }

    fn into_change(self) -> Change {
        Change::RestartPolicy(self)
    }

    fn parse_interactive(service_name: &str) -> Result<Vec<Self>> {
        todo!()
    }

    fn parse_non_interactive(args: Vec<Vec<String>>) -> Vec<(String, Self)> {
        args.iter().filter_map(|chunk| {
            // always minimum of 2 arguments
            let service = chunk.first().unwrap().to_owned();
            let Ok(kind) = RestartPolicyTypes::from_str(chunk.get(1).unwrap()) else {
                eprintln!(
                    "Invalid restart policy type. Valid types are: {} (skipping)",
                    RestartPolicyTypes::iter()
                        .map(|f| f.to_string().to_lowercase())
                        .collect::<Vec<String>>()
                        .join(", ")
                );
                return None;
            };
            Some((service, match kind {
                RestartPolicyTypes::Always => RestartPolicy::Always,
                RestartPolicyTypes::Never => RestartPolicy::Never,
                RestartPolicyTypes::OnFailure => {
                    // third argument must be scecified
                    if let Some(max_retries) = chunk.get(2) && let Ok(parsed) = max_retries.parse::<u16>() {
                        RestartPolicy::OnFailure(parsed)
                    } else {
                        eprintln!("Restart policy type on failure requires max retries. e.g --service-restart-policy <service> on_failure 10");
                        return None;
                    }

                }
            }))
        }).collect()
    }
}
