use super::*;
pub use crate::controllers::variables::Variable;
use crate::util::prompt::prompt_variables;

impl ChangeHandler for Variable {
    fn get_args(args: &EnvironmentConfigOptions) -> Vec<Vec<String>> {
        chunk(&args.service_variables, 2)
    }

    fn parse_non_interactive(args: Vec<Vec<String>>) -> Vec<(String, Variable)> {
        args.iter()
            .filter_map(|chunk| {
                // clap ensures that there will always be 2 values whenever the flag is provided
                // this is unfiltered user input. validation of the service happens in the edit_services_select function
                let service = chunk.first()?.to_owned();

                let variable = match chunk.last()?.parse::<Variable>() {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("{e:?} (skipping)");
                        return None;
                    }
                };

                Some((service, variable))
            })
            .collect()
    }

    fn parse_interactive(service_name: &str) -> Result<Vec<Variable>> {
        prompt_variables(Some(service_name))
    }

    fn into_change(self) -> Change {
        Change::Variable(self)
    }
}
