use super::*;
use crate::util::prompt::{prompt_options, prompt_text};
use std::str::FromStr as _;
use strum::{EnumDiscriminants, EnumString, IntoEnumIterator};

#[derive(Clone, Debug, EnumDiscriminants, DeriveDisplay)]
#[strum_discriminants(derive(Display, EnumIter, EnumString), name(SourceTypes))]
pub enum Source {
    #[strum_discriminants(strum(
        to_string = "Docker image",
        serialize = "docker",
        serialize = "image"
    ))]
    #[display("Docker: {}", _0)]
    Docker(String),

    #[strum_discriminants(strum(
        to_string = "GitHub repo",
        serialize = "github",
        serialize = "git",
        serialize = "gh"
    ))]
    #[display("GitHub: {}/{}/{}", owner, repo, branch)]
    GitHub {
        owner: String,
        repo: String,
        branch: String,
    },
}

fn parse_repo(repo: String) -> Result<Source> {
    let s = repo
        .splitn(3, '/')
        .filter(|&s| !s.is_empty())
        .map(|s| s.to_string())
        .collect::<Vec<String>>();
    match s.len() {
        3 => Ok(Source::GitHub {
            owner: s.first().unwrap().to_string(),
            repo: s.get(1).unwrap().to_string(),
            branch: s.get(2).unwrap().to_string(),
        }),
        _ => anyhow::bail!("malformed repo: <owner>/<repo>/<branch>"),
    }
}

impl ChangeHandler for Source {
    fn get_args(args: &EnvironmentConfigOptions) -> Vec<Vec<String>> {
        chunk(&args.service_sources, 3)
    }

    fn parse_non_interactive(args: Vec<Vec<String>>) -> Vec<(String, Source)> {
        args.iter()
            .filter_map(|chunk| {
                // clap ensures that there will always be 3 values whenever the flag is provided
                let service = chunk.first()?.to_owned();

                let source_type = match SourceTypes::from_str(&chunk.get(1)?.to_lowercase()) {
                    Ok(f) => f,
                    Err(_) => {
                        eprintln!(
                            "Invalid platform. Valid platforms are: {} (skipping)",
                            SourceTypes::iter()
                                .map(|f| f.to_string())
                                .collect::<Vec<String>>()
                                .join(", ")
                        );
                        return None;
                    }
                };

                let source = match source_type {
                    SourceTypes::Docker => Some(Source::Docker(chunk.last()?.to_string())),
                    SourceTypes::GitHub => match parse_repo(chunk.last()?.to_string()) {
                        Ok(source) => Some(source),
                        Err(e) => {
                            eprintln!("{:?} (skipping)", e);
                            return None;
                        }
                    },
                }?;

                Some((service, source))
            })
            .collect()
    }

    fn parse_interactive(service_name: &str) -> Result<Vec<Source>> {
        let source_type = prompt_options(
            &format!("What type of source for {}?", service_name),
            SourceTypes::iter().collect(),
        )?;

        let source = match source_type {
            SourceTypes::Docker => {
                let image = prompt_text("Enter docker image")?;
                Source::Docker(image)
            }
            SourceTypes::GitHub => {
                let repo = prompt_text("Enter repo (owner/repo/branch)")?;
                parse_repo(repo)?
            }
        };

        Ok(vec![source])
    }

    fn into_change(self) -> Change {
        Change::Source(self)
    }
}
