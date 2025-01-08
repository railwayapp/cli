use std::fmt::Display;

use crate::util::prompt::prompt_select;

use super::{queries::user_projects::UserProjectsMeTeamsEdgesNode, *};

/// Create a new project
#[derive(Parser)]
#[clap(alias = "new")]
pub struct Args {
    #[clap(short, long)]
    /// Project name
    name: Option<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let vars = queries::user_projects::Variables {};
    let me = post_graphql::<queries::UserProjects, _>(&client, configs.get_backboard(), vars)
        .await?
        .me;

    let teams: Vec<_> = me.teams.edges.iter().map(|team| &team.node).collect();
    let team_names = get_team_names(teams);
    let team = prompt_team(team_names)?;

    let project_name = match args.name {
        Some(name) => name,
        None => prompt_project_name()?,
    };

    let team_id = match team {
        Team::Team(team) => Some(team.id.clone()),
        _ => None,
    };

    let vars = mutations::project_create::Variables {
        name: Some(project_name),
        description: None,
        team_id,
    };
    let project_create =
        post_graphql::<mutations::ProjectCreate, _>(&client, configs.get_backboard(), vars)
            .await?
            .project_create;

    let environment = project_create
        .environments
        .edges
        .first()
        .context("No environments")?
        .node
        .clone();

    configs.link_project(
        project_create.id.clone(),
        Some(project_create.name.clone()),
        environment.id,
        Some(environment.name),
    )?;
    configs.write()?;

    println!(
        "{} {} on {}",
        "Created project".green().bold(),
        project_create.name.bold(),
        team
    );

    println!(
        "{}",
        format!(
            "https://{}/project/{}",
            configs.get_host(),
            project_create.id
        )
        .bold()
        .underline()
    );
    Ok(())
}

fn get_team_names(teams: Vec<&UserProjectsMeTeamsEdgesNode>) -> Vec<Team> {
    let mut team_names = teams
        .iter()
        .map(|team| Team::Team(team))
        .collect::<Vec<_>>();
    team_names.insert(0, Team::Personal);
    team_names
}

fn prompt_team(teams: Vec<Team>) -> Result<Team> {
    // If there is only the personal team, return None
    if teams.len() == 1 {
        return Ok(Team::Personal);
    }
    let team = prompt_select("Team", teams)?;
    Ok(team)
}

fn prompt_project_name() -> Result<String> {
    // Need a custom inquire prompt here, because of the formatter
    let maybe_name = inquire::Text::new("Project Name")
        .with_formatter(&|s| {
            if s.is_empty() {
                "Will be randomly generated".to_string()
            } else {
                s.to_string()
            }
        })
        .with_placeholder("my-first-project")
        .with_help_message("Leave blank to generate a random name")
        .with_render_config(Configs::get_render_config())
        .prompt()?;

    // If name is empty, generate a random name
    let name = match maybe_name.as_str() {
        "" => {
            use names::Generator;
            let mut generator = Generator::default();
            generator.next().context("Failed to generate name")?
        }
        _ => maybe_name,
    };

    Ok(name)
}

#[derive(Debug, Clone)]
enum Team<'a> {
    Team(&'a UserProjectsMeTeamsEdgesNode),
    Personal,
}

impl Display for Team<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Team::Team(team) => write!(f, "{}", team.name),
            Team::Personal => write!(f, "{}", "Personal".bold()),
        }
    }
}
