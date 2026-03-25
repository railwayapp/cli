use std::{
    collections::BTreeMap,
    fs::{self, File, create_dir_all},
    io::Read,
    path::PathBuf,
};

use anyhow::{Context, Result, anyhow, bail};
use colored::Colorize;
use inquire::ui::{Attributes, RenderConfig, StyleSheet, Styled};
use serde::{Deserialize, Serialize};

use crate::{
    client::{GQLClient, post_graphql},
    commands::queries,
    consts,
    errors::RailwayError,
};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde_with::skip_serializing_none]
#[serde(rename_all = "camelCase")]
pub struct LinkedProject {
    pub project_path: String,
    pub name: Option<String>,
    pub project: String,
    pub environment: String,
    pub environment_name: Option<String>,
    pub service: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
#[serde_with::skip_serializing_none]
#[serde(rename_all = "camelCase")]
pub struct RailwayUser {
    pub token: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub token_expires_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde_with::skip_serializing_none]
#[serde(rename_all = "camelCase")]
pub struct RailwayConfig {
    pub projects: BTreeMap<String, LinkedProject>,
    pub user: RailwayUser,
    /// (path, id)
    pub linked_functions: Option<Vec<(String, String)>>,
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
                        user: RailwayUser::default(),
                        linked_functions: None,
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
                user: RailwayUser::default(),
                linked_functions: None,
            },
        })
    }

    pub fn reset(&mut self) -> Result<()> {
        self.root_config = RailwayConfig {
            projects: BTreeMap::new(),
            user: RailwayUser::default(),
            linked_functions: None,
        };
        Ok(())
    }

    pub fn get_railway_token() -> Option<String> {
        std::env::var(consts::RAILWAY_TOKEN_ENV).ok()
    }

    pub fn get_railway_api_token() -> Option<String> {
        std::env::var(consts::RAILWAY_API_TOKEN_ENV).ok()
    }

    pub fn get_railway_project_id() -> Option<String> {
        std::env::var(consts::RAILWAY_PROJECT_ID_ENV).ok()
    }

    pub fn get_railway_environment_id() -> Option<String> {
        std::env::var(consts::RAILWAY_ENVIRONMENT_ID_ENV).ok()
    }

    pub fn get_railway_service_id() -> Option<String> {
        std::env::var(consts::RAILWAY_SERVICE_ID_ENV).ok()
    }

    /// Returns true if RAILWAY_PROJECT_ID and RAILWAY_ENVIRONMENT_ID env vars are both set,
    /// allowing the link step to be skipped entirely.
    pub fn has_env_var_project_config() -> bool {
        Self::get_railway_project_id().is_some() && Self::get_railway_environment_id().is_some()
    }

    /// Returns true if using token-based auth (RAILWAY_TOKEN or RAILWAY_API_TOKEN)
    /// rather than session-based auth from `railway login`.
    /// Token-based auth bypasses 2FA on the backend, so client-side 2FA checks are unnecessary.
    pub fn is_using_token_auth() -> bool {
        Self::get_railway_token().is_some() || Self::get_railway_api_token().is_some()
    }

    pub fn env_is_ci() -> bool {
        std::env::var("CI")
            .map(|val| val.trim().to_lowercase() == "true")
            .unwrap_or(false)
    }

    /// tries the environment variable and the config file
    pub fn get_railway_auth_token(&self) -> Option<String> {
        Self::get_railway_api_token()
            .or(self
                .root_config
                .user
                .access_token
                .clone()
                .filter(|t| !t.is_empty()))
            .or(self
                .root_config
                .user
                .token
                .clone()
                .filter(|t| !t.is_empty()))
    }

    pub fn has_oauth_token(&self) -> bool {
        self.root_config.user.access_token.is_some()
    }

    pub fn get_refresh_token(&self) -> Option<&str> {
        self.root_config.user.refresh_token.as_deref()
    }

    pub fn is_token_expired(&self) -> bool {
        match self.root_config.user.token_expires_at {
            Some(expires_at) => {
                let now = chrono::Utc::now().timestamp();
                now >= (expires_at - 60) // 60s buffer
            }
            None => false,
        }
    }

    pub fn save_oauth_tokens(
        &mut self,
        access_token: &str,
        refresh_token: Option<&str>,
        expires_in: i64,
    ) -> Result<()> {
        anyhow::ensure!(!access_token.is_empty(), "access_token cannot be empty");
        anyhow::ensure!(expires_in > 0, "Server returned non-positive expires_in");
        let expires_at = chrono::Utc::now().timestamp() + expires_in;
        self.root_config.user.access_token = Some(access_token.to_string());
        self.root_config.user.refresh_token = refresh_token.map(|s| s.to_string());
        self.root_config.user.token_expires_at = Some(expires_at);
        self.root_config.user.token = None; // Clear legacy token
        self.write()
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
            Environment::Production => "railway.com",
            Environment::Staging => "railway-staging.com",
            Environment::Dev => "railway-develop.com",
        }
    }

    /// Returns the host and path for relay server without protocol (e.g. "backboard.railway.com/relay")
    /// Protocol is omitted to allow flexibility between https:// and wss:// usage
    pub fn get_relay_host_path(&self) -> String {
        format!("backboard.{}/relay", self.get_host())
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
        if Self::has_env_var_project_config() || Self::get_railway_token().is_some() {
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

        Err(RailwayError::NoLinkedProject.into())
    }

    /// Returns the locally-linked project from disk config, ignoring any RAILWAY_TOKEN override.
    pub fn get_local_linked_project(&self) -> Result<LinkedProject> {
        let mut current_path = std::env::current_dir()?;
        loop {
            let path = current_path
                .to_str()
                .context("Unable to get current working directory")?
                .to_owned();
            if let Some(project) = self.root_config.projects.get(&path) {
                return Ok(project.clone());
            }
            if !current_path.pop() {
                break;
            }
        }
        Err(RailwayError::NoLinkedProject.into())
    }

    pub async fn get_linked_project(&self) -> Result<LinkedProject> {
        let path = self.get_closest_linked_project_directory()?;
        let project = self.root_config.projects.get(&path);

        if Self::get_railway_token().is_some() {
            let vars = queries::project_token::Variables {};
            let client = GQLClient::new_authorized(self)?;

            let data =
                post_graphql::<queries::ProjectToken, _>(&client, self.get_backboard(), vars)
                    .await?;

            let project = LinkedProject {
                project_path: self.get_current_directory()?,
                name: Some(data.project_token.project.name),
                project: data.project_token.project.id,
                environment: data.project_token.environment.id,
                environment_name: Some(data.project_token.environment.name),
                service: project.cloned().and_then(|p| p.service),
            };
            return Ok(project);
        }

        if let (Some(project_id), Some(environment_id)) = (
            Self::get_railway_project_id(),
            Self::get_railway_environment_id(),
        ) {
            if self.get_railway_auth_token().is_none() {
                bail!(RailwayError::Unauthorized);
            }

            let service_id =
                Self::get_railway_service_id().or_else(|| project.cloned().and_then(|p| p.service));

            return Ok(LinkedProject {
                project_path: self.get_current_directory()?,
                name: None,
                project: project_id,
                environment: environment_id,
                environment_name: None,
                service: service_id,
            });
        }

        project
            .cloned()
            .ok_or_else(|| RailwayError::NoLinkedProject.into())
    }

    pub fn get_linked_project_mut(&mut self) -> Result<&mut LinkedProject> {
        let path = self.get_closest_linked_project_directory()?;
        let project = self.root_config.projects.get_mut(&path);

        project.ok_or_else(|| RailwayError::ProjectNotFound.into())
    }

    pub fn link_project(
        &mut self,
        project_id: String,
        name: Option<String>,
        environment_id: String,
        environment_name: Option<String>,
    ) -> Result<()> {
        let path = self.get_current_directory()?;
        let project = LinkedProject {
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

    pub fn unlink_project(&mut self) {
        if let Ok(path) = self.get_closest_linked_project_directory() {
            self.root_config.projects.remove(&path);
        }
    }

    pub fn unlink_service(&mut self) -> Result<()> {
        let linked_project = self.get_linked_project_mut()?;
        linked_project.service = None;
        Ok(())
    }

    pub fn link_function(&mut self, path: PathBuf, id: String) -> Result<()> {
        let path = path
            .canonicalize()?
            .to_str()
            .ok_or(anyhow!("couldn't convert string"))?
            .to_owned();
        let functions = self
            .root_config
            .linked_functions
            .get_or_insert_with(Vec::new);
        functions.retain(|(p, i)| (path != *p) && (id != *i));
        functions.push((path, id));
        Ok(())
    }

    pub fn get_function(&self, path: PathBuf) -> Result<Option<String>> {
        let canonical_path = path.canonicalize()?;
        let path_str = canonical_path
            .to_str()
            .ok_or(anyhow!("couldn't convert string"))?;

        if let Some(functions) = &self.root_config.linked_functions {
            Ok(functions.iter().find_map(|(p, id)| {
                if p == path_str {
                    Some(id.clone())
                } else {
                    None
                }
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_functions_in_directory(&self, path: PathBuf) -> Result<Vec<(PathBuf, String)>> {
        let canonical_path = path.canonicalize()?;
        let path_str = canonical_path
            .to_str()
            .ok_or(anyhow!("couldn't convert string"))?;
        if let Some(functions) = &self.root_config.linked_functions {
            Ok(functions
                .iter()
                .filter_map(|(p, id)| {
                    if p.starts_with(path_str) {
                        let p = PathBuf::from(p);
                        if p.exists() {
                            Some((p, id.clone()))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .collect())
        } else {
            Ok(Vec::new())
        }
    }

    pub fn unlink_function(&mut self, id: String) -> Result<()> {
        if let Some(functions) = &mut self.root_config.linked_functions {
            if let Some(pos) = functions.iter().position(|(_, i)| *i == id) {
                functions.swap_remove(pos);
                functions.retain(|(_, i)| *i != id);
            }
        }
        Ok(())
    }

    pub fn get_render_config() -> RenderConfig<'static> {
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
            .with_canceled_prompt_indicator(
                Styled::new("<cancelled>").with_fg(inquire::ui::Color::DarkRed),
            )
    }

    pub fn write(&self) -> Result<()> {
        let config_dir = self
            .root_config_path
            .parent()
            .context("Failed to get parent directory")?;

        // Ensure directory exists
        create_dir_all(config_dir)?;

        // Use temporary file to achieve atomic write:
        //  1. Open file ~/railway/config.tmp
        //  2. Serialize config to temporary file
        //  3. Rename temporary file to ~/railway/config.json (atomic operation)
        let tmp_file_path = self.root_config_path.with_extension("tmp");
        let tmp_file = File::options()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_file_path)?;
        serde_json::to_writer_pretty(&tmp_file, &self.root_config)?;
        tmp_file.sync_all()?;

        // Rename file to final destination to achieve atomic write
        fs::rename(tmp_file_path.as_path(), &self.root_config_path)?;

        Ok(())
    }
}
