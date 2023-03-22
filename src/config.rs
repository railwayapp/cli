use std::{collections::BTreeMap, fs::{create_dir_all, File}, fs, io::Read, path::PathBuf};
use std::env::temp_dir;

use anyhow::{Context, Result};
use colored::Colorize;
use inquire::ui::{Attributes, RenderConfig, Styled, StyleSheet};
use serde::{Deserialize, Serialize};

use crate::{
    client::{GQLClient, post_graphql},
    commands::queries,
};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde_with::skip_serializing_none]
#[serde(rename_all = "camelCase")]
pub struct RailwayProject {
    pub project_path: String,
    pub name: Option<String>,
    pub project: String,
    pub environment: String,
    pub environment_name: Option<String>,
    pub service: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde_with::skip_serializing_none]
#[serde(rename_all = "camelCase")]
pub struct RailwayUser {
    pub token: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde_with::skip_serializing_none]
#[serde(rename_all = "camelCase")]
pub struct RailwayConfig {
    pub projects: BTreeMap<String, RailwayProject>,
    pub user: RailwayUser,
}

#[derive(Debug)]
#[serde_with::skip_serializing_none]
pub struct Configs {
    pub root_config: RailwayConfig,
    root_config_path: PathBuf,
}

pub enum Environment {
    Production,
    Staging,
    Dev,
}

impl Configs {
    pub fn new() -> Result<Self> {
        let environment = Self::get_environment_id();
        let root_config_partial_path = match environment {
            Environment::Production => ".railway/config.json",
            Environment::Staging => ".railway/config-staging.json",
            Environment::Dev => ".railway/config-dev.json",
        };

        let home_dir = dirs::home_dir().context("Unable to get home directory")?;
        let root_config_path = std::path::Path::new(&home_dir).join(root_config_partial_path);

        if let Ok(mut file) = File::open(&root_config_path) {
            let mut serialized_config = vec![];
            file.read_to_end(&mut serialized_config)?;

            let root_config: RailwayConfig = serde_json::from_slice(&serialized_config)
                .unwrap_or_else(|_| {
                    eprintln!("{}", "Unable to parse config file, regenerating".yellow());
                    RailwayConfig {
                        projects: BTreeMap::new(),
                        user: RailwayUser { token: None },
                    }
                });

            let config = Self {
                root_config,
                root_config_path,
            };

            return Ok(config);
        }

        Ok(Self {
            root_config_path,
            root_config: RailwayConfig {
                projects: BTreeMap::new(),
                user: RailwayUser { token: None },
            },
        })
    }

    pub fn reset(&mut self) -> Result<()> {
        self.root_config = RailwayConfig {
            projects: BTreeMap::new(),
            user: RailwayUser { token: None },
        };
        Ok(())
    }

    pub fn get_railway_token() -> Option<String> {
        std::env::var("RAILWAY_TOKEN").ok()
    }

    pub fn get_railway_api_token() -> Option<String> {
        std::env::var("RAILWAY_API_TOKEN").ok()
    }

    pub fn get_environment_id() -> Environment {
        match std::env::var("RAILWAY_ENV")
            .map(|env| env.to_lowercase())
            .as_deref()
        {
            Ok("production") => Environment::Production,
            Ok("staging") => Environment::Staging,
            Ok("dev") => Environment::Dev,
            Ok("develop") => Environment::Dev,
            _ => Environment::Production,
        }
    }

    pub fn get_host(&self) -> &'static str {
        match Self::get_environment_id() {
            Environment::Production => "railway.app",
            Environment::Staging => "railway-staging.app",
            Environment::Dev => "railway-develop.app",
        }
    }

    pub fn get_backboard(&self) -> String {
        format!("https://backboard.{}/graphql/v2", self.get_host())
    }

    pub fn get_current_directory(&self) -> Result<String> {
        let current_dir = std::env::current_dir()?;
        let path = current_dir
            .to_str()
            .context("Unable to get current working directory")?;
        Ok(path.to_owned())
    }

    pub fn get_closest_linked_project_directory(&self) -> Result<String> {
        if Self::get_railway_token().is_some() {
            return self.get_current_directory();
        }
        let mut current_path = std::env::current_dir()?;
        loop {
            let path = current_path
                .to_str()
                .context("Unable to get current working directory")?
                .to_owned();
            let config = self.root_config.projects.get(&path);
            if config.is_some() {
                return Ok(path);
            }
            if !current_path.pop() {
                break;
            }
        }
        Err(anyhow::anyhow!("No linked project found"))
    }

    pub async fn get_linked_project(&self) -> Result<RailwayProject> {
        if Self::get_railway_token().is_some() {
            let vars = queries::project_token::Variables {};
            let client = GQLClient::new_authorized(self)?;

            let res = post_graphql::<queries::ProjectToken, _>(&client, self.get_backboard(), vars)
                .await?;

            let data = res.data.context("Invalid project token!")?;

            let project = RailwayProject {
                project_path: self.get_current_directory()?,
                name: Some(data.project_token.project.name),
                project: data.project_token.project.id,
                environment: data.project_token.environment.id,
                environment_name: Some(data.project_token.environment.name),
                service: None,
            };
            return Ok(project);
        }
        let path = self.get_closest_linked_project_directory()?;
        let project = self
            .root_config
            .projects
            .get(&path)
            .context("Project not found! Run `railway link` to link to a project")?;
        Ok(project.clone())
    }

    pub fn get_linked_project_mut(&mut self) -> Result<&mut RailwayProject> {
        let path = self.get_closest_linked_project_directory()?;
        let project = self
            .root_config
            .projects
            .get_mut(&path)
            .context("Project not found! Run `railway link` to link to a project")?;
        Ok(project)
    }

    pub fn link_project(
        &mut self,
        project_id: String,
        name: Option<String>,
        environment_id: String,
        environment_name: Option<String>,
    ) -> Result<()> {
        let path = self.get_current_directory()?;
        let project = RailwayProject {
            project_path: path.clone(),
            name,
            project: project_id,
            environment: environment_id,
            environment_name,
            service: None,
        };
        self.root_config.projects.insert(path, project);
        Ok(())
    }

    pub fn link_service(&mut self, service_id: String) -> Result<()> {
        let linked_project = self.get_linked_project_mut()?;
        linked_project.service = Some(service_id);
        Ok(())
    }

    pub fn unlink_project(&mut self) -> Result<RailwayProject> {
        let path = self.get_closest_linked_project_directory()?;
        let project = self
            .root_config
            .projects
            .remove(&path)
            .context("Project not found! Run `railway link` to link to a project")?;
        Ok(project)
    }

    pub fn unlink_service(&mut self) -> Result<()> {
        let linked_project = self.get_linked_project_mut()?;
        linked_project.service = None;
        Ok(())
    }

    pub fn get_render_config() -> RenderConfig {
        RenderConfig::default_colored()
            .with_help_message(
                StyleSheet::new()
                    .with_fg(inquire::ui::Color::LightMagenta)
                    .with_attr(Attributes::BOLD),
            )
            .with_answer(
                StyleSheet::new()
                    .with_fg(inquire::ui::Color::LightCyan)
                    .with_attr(Attributes::BOLD),
            )
            .with_prompt_prefix(
                Styled::new("?").with_style_sheet(
                    StyleSheet::new()
                        .with_fg(inquire::ui::Color::LightCyan)
                        .with_attr(Attributes::BOLD),
                ),
            )
    }

    pub fn write(&self) -> Result<()> {
        // Ensure directory exists
        create_dir_all(self.root_config_path.parent().unwrap())?;

        // Create temporary file to write initial data to. This is to ensure updates
        // are atomic. After writingn the tmp file, we will rename it to the final destination,
        // which is an atomic operation.
        let mut tmp_file_path = temp_dir();
        tmp_file_path.push(self.root_config_path.file_name().unwrap());

        let tmp_file = File::options().create(true).write(true).open(&tmp_file_path)?;
        serde_json::to_writer_pretty(&tmp_file, &self.root_config)?;
        tmp_file.sync_all()?;

        // Rename file to final destination to achieve atomic write
        fs::rename(&tmp_file_path.as_path(), &self.root_config_path)?;

        Ok(())
    }
}
