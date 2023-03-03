use std::fmt::Display;

use chrono::format;

use super::{queries::user_projects::UserProjectsMeTeamsEdgesNode, *};

/// Create a new project
#[derive(Parser)]
#[clap(alias = "new")]
pub struct Args {}

pub async fn command(_args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    let render_config = configs.get_render_config();
    let client = GQLClient::new_authorized(&configs)?;
    let _linked_project = configs.get_linked_project().await.ok();

    inquire::Select::new(
        "Starting Point",
        vec![
            "Empty Project",
            // Coming Soon!!
            //  "Start from Template"
        ],
    )
    .with_render_config(render_config)
    .prompt()?;

    let name = inquire::Text::new("Project Name")
        .with_formatter(&|s| {
            if s.is_empty() {
                "Will be randomly generated".to_string()
            } else {
                s.to_string()
            }
        })
        .with_placeholder("my-first-project")
        .with_help_message("Leave blank to generate a random name")
        .with_render_config(render_config)
        .prompt()?;

    let name = if name.is_empty() {
        use names::Generator;
        let mut generator = Generator::default();
        generator.next().context("Failed to generate name")?
    } else {
        name
    };

    let description = inquire::Text::new("Project Description")
        .with_formatter(&|s| {
            if s.is_empty() {
                "No description provided".to_string()
            } else {
                s.to_string()
            }
        })
        .with_placeholder("My first Railway project")
        .with_help_message("Optional")
        .with_render_config(render_config)
        .prompt()?;

    let vars = queries::user_projects::Variables {};

    let res =
        post_graphql::<queries::UserProjects, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;
    let teams: Vec<_> = body.me.teams.edges.iter().map(|team| &team.node).collect();

    if teams.is_empty() {
        let vars = mutations::project_create::Variables {
            name: Some(name),
            description: Some(description),
            team_id: None,
        };

        let res =
            post_graphql::<mutations::ProjectCreate, _>(&client, configs.get_backboard(), vars)
                .await?;

        let body = res.data.context("Failed to retrieve response body")?;
        let environment = body
            .project_create
            .environments
            .edges
            .first()
            .context("No environments")?
            .node
            .clone();
        configs.link_project(
            body.project_create.id.clone(),
            Some(body.project_create.name.clone()),
            environment.id,
            Some(environment.name),
        )?;
        configs.write()?;
        println!(
            "{} {}",
            "Created project".green().bold(),
            body.project_create.name.bold(),
        );
        return Ok(());
    }

    let mut team_names = teams
        .iter()
        .map(|team| Team::Team(team))
        .collect::<Vec<_>>();
    team_names.insert(0, Team::Personal);

    let team = inquire::Select::new("Project Team", team_names)
        .with_render_config(configs.get_render_config())
        .prompt()?;
    let team_id = match team {
        Team::Team(team) => Some(team.id.clone()),
        Team::Personal => None,
    };
    let vars = mutations::project_create::Variables {
        name: Some(name),
        description: Some(description),
        team_id,
    };

    let res =
        post_graphql::<mutations::ProjectCreate, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;
    let environment = body
        .project_create
        .environments
        .edges
        .first()
        .context("No environments")?
        .node
        .clone();
    configs.link_project(
        body.project_create.id.clone(),
        Some(body.project_create.name.clone()),
        environment.id,
        Some(environment.name),
    )?;
    configs.write()?;
    println!(
        "{} {} on {}",
        "Created project".green().bold(),
        body.project_create.name.bold(),
        team
    );
    println!(
        "{}",
        format!(
            "https://{}/project/{}",
            configs.get_host(),
            body.project_create.id
        )
        .bold()
        .underline()
    );
    Ok(())
}

#[derive(Debug, Clone)]
enum Team<'a> {
    Team(&'a UserProjectsMeTeamsEdgesNode),
    Personal,
}

impl<'a> Display for Team<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Team::Team(team) => write!(f, "{}", team.name),
            Team::Personal => write!(f, "{}", "Personal".bold()),
        }
    }
}
