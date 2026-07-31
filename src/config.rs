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
    pub environment: Option<String>,
    pub environment_name: Option<String>,
    pub service: Option<String>,
}

impl LinkedProject {
    /// Returns the environment ID, or an error if no environment is linked.
    pub fn environment_id(&self) -> Result<&str> {
        self.environment.as_deref().ok_or_else(|| {
            anyhow!(
                "No environment specified. Set RAILWAY_ENVIRONMENT_ID, use --environment, or run `railway environment` to link one."
            )
        })
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
#[serde_with::skip_serializing_none]
#[serde(rename_all = "camelCase")]
pub struct RailwayUser {
    pub id: Option<String>,
    pub token: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub token_expires_at: Option<i64>,
}

/// A sandbox the CLI has created or seen, cached locally so `railway sandbox
/// ssh`/`exec`/`destroy` can recover its environment (the connection string is
/// `sbx:<environmentId>:<id>`) without re-specifying `--environment`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde_with::skip_serializing_none]
#[serde(rename_all = "camelCase")]
pub struct StoredSandbox {
    pub id: String,
    pub environment_id: String,
    pub project_id: Option<String>,
    pub created_at: Option<String>,
}

/// A sandbox template recipe the CLI has built. Templates are
/// content-addressed server-side (the id is a hash of the recipe) and
/// `sandboxCreate` needs the full recipe — not just the id — so the CLI keeps
/// the instructions locally to make `railway sandbox create --template <name>`
/// possible.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde_with::skip_serializing_none]
#[serde(rename_all = "camelCase")]
pub struct StoredSandboxTemplate {
    /// Server-side template id (sha256 of the recipe).
    pub id: String,
    /// Optional local-only name for friendlier lookup.
    pub name: Option<String>,
    pub environment_id: String,
    pub instructions: Vec<String>,
    pub base_image_digest: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
#[serde_with::skip_serializing_none]
#[serde(rename_all = "camelCase")]
pub struct RailwayConfig {
    pub projects: BTreeMap<String, LinkedProject>,
    pub user: RailwayUser,
    pub editor: Option<String>,
    /// (path, id)
    pub linked_functions: Option<Vec<(String, String)>>,
    /// Sandboxes the CLI knows about (id -> environment cache).
    pub sandboxes: Option<Vec<StoredSandbox>>,
    /// The most recently created/used sandbox; the default target for
    /// `railway sandbox ssh` when no id is given.
    pub active_sandbox: Option<String>,
    /// Sandbox template recipes the CLI has built (id is server-side hash;
    /// instructions kept locally because sandboxCreate needs the full recipe).
    pub sandbox_templates: Option<Vec<StoredSandboxTemplate>>,
    /// The cloud agent `railway code` last used, per environment. Unlike a
    /// sandbox this is a durable box the user comes back to, so the pointer is
    /// keyed by environment rather than being a single global "active" slot:
    /// switching projects must not make `railway code` reach for a box in a
    /// different environment.
    pub code_agents: Option<BTreeMap<String, String>>,
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
        let root_config_path = Self::root_config_path()?;

        if let Ok(mut file) = File::open(&root_config_path) {
            let mut serialized_config = vec![];
            file.read_to_end(&mut serialized_config)?;

            let root_config: RailwayConfig = serde_json::from_slice(&serialized_config)
                .unwrap_or_else(|_| {
                    eprintln!("{}", "Unable to parse config file, regenerating".yellow());
                    RailwayConfig::default()
                });

            let config = Self {
                root_config,
                root_config_path,
            };

            return Ok(config);
        }

        Ok(Self {
            root_config_path,
            root_config: RailwayConfig::default(),
        })
    }

    /// Absolute path to the root config file for the current environment.
    fn root_config_path() -> Result<PathBuf> {
        let root_config_partial_path = match Self::get_environment_id() {
            Environment::Production => ".railway/config.json",
            Environment::Staging => ".railway/config-staging.json",
            Environment::Dev => ".railway/config-dev.json",
        };

        let home_dir = dirs::home_dir().context("Unable to get home directory")?;
        Ok(std::path::Path::new(&home_dir).join(root_config_partial_path))
    }

    /// Re-read the root config from disk, discarding any in-memory state.
    /// Used after acquiring the config lock so a refresh sees credentials
    /// freshly written by another concurrent process.
    pub fn reload(&mut self) -> Result<()> {
        self.root_config = Self::read_root_config(&self.root_config_path).unwrap_or_default();
        Ok(())
    }

    pub fn reset(&mut self) -> Result<()> {
        self.root_config = RailwayConfig::default();
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

    /// Returns true if either RAILWAY_PROJECT_ID or RAILWAY_ENVIRONMENT_ID env vars are set,
    /// indicating the user intends to use env-var-based project targeting.
    pub fn has_env_var_project_config() -> bool {
        Self::get_railway_project_id().is_some() || Self::get_railway_environment_id().is_some()
    }

    /// Returns true if using token-based auth (RAILWAY_TOKEN or RAILWAY_API_TOKEN)
    /// rather than session-based auth from `railway login`.
    /// Token-based auth bypasses 2FA on the backend, so client-side 2FA checks are unnecessary.
    pub fn is_using_token_auth() -> bool {
        Self::get_railway_token().is_some() || Self::get_railway_api_token().is_some()
    }

    /// True when the `CI` env var is set to any truthy value (`true`,
    /// `1`, `yes`, ...). Runners differ on the value they export, so
    /// treat anything except empty / `false` / `0` as CI — consistent
    /// with `is_likely_headless`, which keys off `CI` being set at all.
    pub fn env_is_ci() -> bool {
        std::env::var("CI")
            .map(|val| {
                let val = val.trim().to_lowercase();
                !val.is_empty() && val != "false" && val != "0"
            })
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

    /// True when any CLI-supported credential is present. This includes
    /// project tokens, so use this for auth preflight checks only; commands
    /// that need user/workspace auth should call `get_railway_auth_token`.
    pub fn has_auth_credentials(&self) -> bool {
        Self::get_railway_token().is_some() || self.get_railway_auth_token().is_some()
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
        // Only overwrite the stored refresh token when the server actually
        // returned a new one. A 200 response that omits `refresh_token` means
        // the existing refresh token is still valid, so preserve it rather than
        // nulling it out (which would force a re-login on the next refresh).
        if let Some(refresh_token) = refresh_token {
            self.root_config.user.refresh_token = Some(refresh_token.to_string());
        }
        self.root_config.user.token_expires_at = Some(expires_at);
        self.root_config.user.token = None; // Clear legacy token
        self.write_credentials()
    }

    /// Drop the stored OAuth credentials after the server has told us they
    /// are permanently dead (`invalid_grant`).
    ///
    /// Without this the CLI re-presents the same revoked refresh token on
    /// every single invocation, forever: the refresh fails, the stale access
    /// token is used anyway, and the user sees a generic "Unauthorized" with
    /// no way out but deleting `~/.railway` by hand. Clearing turns that
    /// permanent wedge into one clean `railway login`.
    ///
    /// Deliberately narrower than `reset()`: only the credential fields are
    /// touched, so the linked project/environment/service survive and the
    /// user lands back where they were after logging in.
    pub fn clear_oauth_tokens(&mut self) -> Result<()> {
        self.root_config.user.access_token = None;
        self.root_config.user.refresh_token = None;
        self.root_config.user.token_expires_at = None;
        // `get_railway_auth_token` falls back to the legacy `token`, so leaving
        // it behind would keep an old install looking authenticated after the
        // clear.
        self.root_config.user.token = None;
        self.write_credentials()
    }

    pub fn save_user_id(&mut self, id: &str) -> Result<()> {
        anyhow::ensure!(!id.is_empty(), "user id cannot be empty");
        self.root_config.user.id = Some(id.to_string());
        self.write()
    }

    /// Build a `Configs` backed by an explicit path, for tests that need to
    /// exercise the real read/modify/write cycle without touching `$HOME`.
    #[cfg(test)]
    pub(crate) fn for_test(root_config_path: PathBuf) -> Self {
        Self {
            root_config: RailwayConfig::default(),
            root_config_path,
        }
    }

    /// Take the exclusive config lock covering this config file, without
    /// blocking the async runtime.
    ///
    /// Acquisition polls with a blocking sleep, so under contention it would
    /// park a tokio worker for up to `CONFIG_LOCK_TIMEOUT` and starve unrelated
    /// tasks — the long-running MCP server serves tool calls concurrently, so
    /// that is a real stall, not a theoretical one.
    pub(crate) async fn acquire_lock(&self) -> ConfigLock {
        let path = self.root_config_path.clone();
        tokio::task::spawn_blocking(move || ConfigLock::acquire(&path))
            .await
            .unwrap_or(ConfigLock { file: None })
    }

    /// Read and parse the root config, or `None` when it is absent or corrupt.
    fn read_root_config(path: &std::path::Path) -> Option<RailwayConfig> {
        let mut file = File::open(path).ok()?;
        let mut buf = vec![];
        file.read_to_end(&mut buf).ok()?;
        serde_json::from_slice(&buf).ok()
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

    pub fn get_backboard(&self) -> String {
        format!("https://backboard.{}/graphql/v2", self.get_host())
    }

    /// SSH relay host and non-default port for the current environment.
    /// Mirrors backboard's `controllers/ssh` mapping: only the develop relay
    /// is separate (and listens on 2222); staging falls through to the
    /// production relay, same as backboard's IS_DEV-only branch.
    pub fn get_ssh_relay() -> (&'static str, Option<u16>) {
        match Self::get_environment_id() {
            Environment::Dev => ("ssh.railway-develop.com", Some(2222)),
            Environment::Production | Environment::Staging => ("ssh.railway.com", None),
        }
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
                environment: Some(data.project_token.environment.id),
                environment_name: Some(data.project_token.environment.name),
                service: project.cloned().and_then(|p| p.service),
            };
            return Ok(project);
        }

        if let Some(resolved) = Self::resolve_env_var_project()? {
            if self.get_railway_auth_token().is_none() {
                bail!(RailwayError::Unauthorized);
            }

            // Only merge local config when it targets the same project,
            // to avoid silently mixing project A's environment with project B.
            // Walk ancestor directories so nested dirs still find the local link.
            let local = self
                .get_local_linked_project()
                .ok()
                .filter(|p| p.project == resolved.project_id);
            let service_id = Self::get_railway_service_id()
                .or_else(|| local.as_ref().and_then(|p| p.service.clone()));

            let env_from_override = resolved.environment_id.is_some();
            let environment = resolved
                .environment_id
                .or_else(|| local.as_ref().and_then(|p| p.environment.clone()));
            // Only carry the local environment name when we fell back to the
            // local environment ID. If the override supplied its own ID, the
            // local name would refer to a different environment.
            let environment_name = if !env_from_override && environment.is_some() {
                local.as_ref().and_then(|p| p.environment_name.clone())
            } else {
                None
            };

            return Ok(LinkedProject {
                project_path: self.get_current_directory()?,
                name: None,
                project: resolved.project_id,
                environment,
                environment_name,
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
            environment: Some(environment_id),
            environment_name,
            service: None,
        };

        self.root_config.projects.insert(path, project);
        Ok(())
    }

    /// Record a sandbox the CLI created/saw. When `set_active` is true it also
    /// becomes the default target for `railway sandbox ssh`. Caller persists
    /// with `write()`.
    pub fn upsert_sandbox(&mut self, sandbox: StoredSandbox, set_active: bool) {
        let id = sandbox.id.clone();
        let sandboxes = self.root_config.sandboxes.get_or_insert_with(Vec::new);
        match sandboxes.iter_mut().find(|s| s.id == sandbox.id) {
            Some(existing) => *existing = sandbox,
            None => sandboxes.push(sandbox),
        }
        if set_active {
            self.root_config.active_sandbox = Some(id);
        }
    }

    /// The active sandbox (most recently created/used), if it is still known.
    pub fn get_active_sandbox(&self) -> Option<StoredSandbox> {
        let id = self.root_config.active_sandbox.as_ref()?;
        self.get_sandbox(id)
    }

    /// Look up a known sandbox by id.
    pub fn get_sandbox(&self, id: &str) -> Option<StoredSandbox> {
        self.root_config
            .sandboxes
            .as_ref()?
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    /// Mark a known sandbox active. Caller persists with `write()`.
    pub fn set_active_sandbox(&mut self, id: &str) {
        self.root_config.active_sandbox = Some(id.to_string());
    }

    /// Forget a sandbox (e.g. after destroy), clearing the active pointer if it
    /// referenced this id. Caller persists with `write()`.
    pub fn remove_sandbox(&mut self, id: &str) {
        if let Some(sandboxes) = self.root_config.sandboxes.as_mut() {
            sandboxes.retain(|s| s.id != id);
        }
        if self.root_config.active_sandbox.as_deref() == Some(id) {
            self.root_config.active_sandbox = None;
        }
    }

    /// The cloud agent `railway code` last used in this environment, if any.
    pub fn get_code_agent(&self, environment_id: &str) -> Option<String> {
        self.root_config
            .code_agents
            .as_ref()?
            .get(environment_id)
            .cloned()
    }

    /// Remember the cloud agent `railway code` is using in this environment.
    /// Caller persists with `write()`.
    pub fn set_code_agent(&mut self, environment_id: &str, id: &str) {
        self.root_config
            .code_agents
            .get_or_insert_with(BTreeMap::new)
            .insert(environment_id.to_string(), id.to_string());
    }

    /// Forget the cloud agent pointer for an environment (the agent was deleted
    /// or has gone unreachable). Caller persists with `write()`.
    pub fn remove_code_agent(&mut self, environment_id: &str) {
        if let Some(agents) = self.root_config.code_agents.as_mut() {
            agents.remove(environment_id);
        }
    }

    /// Record a sandbox template recipe (upsert by template id within the same
    /// environment). When a name is given, any other template in the
    /// environment holding that name loses it — names are unique handles.
    /// Caller persists with `write()`.
    pub fn upsert_sandbox_template(&mut self, template: StoredSandboxTemplate) {
        let templates = self
            .root_config
            .sandbox_templates
            .get_or_insert_with(Vec::new);
        if let Some(name) = &template.name {
            for other in templates.iter_mut() {
                if other.environment_id == template.environment_id
                    && other.id != template.id
                    && other.name.as_deref() == Some(name)
                {
                    other.name = None;
                }
            }
        }
        match templates
            .iter_mut()
            .find(|t| t.id == template.id && t.environment_id == template.environment_id)
        {
            Some(existing) => *existing = template,
            None => templates.push(template),
        }
    }

    /// Look up a stored template by local name or id (exact or unambiguous id
    /// prefix), optionally scoped to an environment.
    pub fn find_sandbox_template(
        &self,
        name_or_id: &str,
        environment_id: Option<&str>,
    ) -> Option<StoredSandboxTemplate> {
        let templates = self.root_config.sandbox_templates.as_ref()?;
        let in_env =
            |t: &&StoredSandboxTemplate| environment_id.is_none_or(|env| t.environment_id == env);
        if let Some(t) = templates
            .iter()
            .filter(in_env)
            .find(|t| t.name.as_deref() == Some(name_or_id))
        {
            return Some(t.clone());
        }
        let mut matches = templates
            .iter()
            .filter(in_env)
            .filter(|t| t.id.starts_with(name_or_id));
        match (matches.next(), matches.next()) {
            (Some(t), None) => Some(t.clone()),
            _ => None,
        }
    }

    /// All stored templates, optionally scoped to an environment.
    pub fn list_sandbox_templates(
        &self,
        environment_id: Option<&str>,
    ) -> Vec<StoredSandboxTemplate> {
        self.root_config
            .sandbox_templates
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|t| environment_id.is_none_or(|env| t.environment_id == env))
            .cloned()
            .collect()
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

    /// Persist the config, taking the credential fields from disk rather than
    /// from this (possibly stale) in-memory snapshot.
    ///
    /// A process can hold a `Configs` for a long time — `railway mcp` keeps one
    /// for the whole editor session — and then write it for an unrelated reason
    /// such as linking a project. Serialising its whole snapshot would put
    /// whatever credentials it loaded at startup back on disk, undoing a token
    /// refresh (or a `clear_oauth_tokens` after a dead grant) performed by
    /// another process in the meantime. Credentials belong to the auth paths, so
    /// an ordinary write never carries them: see [`Self::write_credentials`].
    pub fn write(&self) -> Result<()> {
        let mut to_write = serde_json::to_value(&self.root_config)?;
        // Re-read immediately before writing so the window in which a
        // concurrent refresh could be lost is microseconds rather than the
        // lifetime of this `Configs`.
        if let Some(disk) = Self::read_root_config(&self.root_config_path) {
            // Merge on the typed struct so the field set is checked by the
            // compiler: a new credential field on `RailwayUser` is picked up
            // automatically instead of being silently dropped. Everything that
            // is not a credential (`id`) stays owned by this caller, so
            // `save_user_id` still works.
            let merged = RailwayUser {
                id: self.root_config.user.id.clone(),
                ..disk.user
            };
            to_write["user"] = serde_json::to_value(merged)?;
        }
        self.write_value(&to_write)
    }

    /// Persist the config including this instance's credential fields.
    ///
    /// Only the auth paths may claim that ownership: login/refresh
    /// ([`Self::save_oauth_tokens`]), a dead grant
    /// ([`Self::clear_oauth_tokens`]), and logout.
    pub(crate) fn write_credentials(&self) -> Result<()> {
        let value = serde_json::to_value(&self.root_config)?;
        self.write_value(&value)
    }

    fn write_value(&self, value: &serde_json::Value) -> Result<()> {
        let config_dir = self
            .root_config_path
            .parent()
            .context("Failed to get parent directory")?;

        // Ensure directory exists
        create_dir_all(config_dir)?;
        // This file holds the OAuth access and refresh tokens, so its mode is a
        // security invariant rather than something to inherit from the ambient
        // umask — the common 022 would leave it world-readable.
        secure_config_dir(config_dir)?;

        // Use a temporary file to achieve an atomic write. The name is unique per
        // writer — matching `util::write_atomic`'s pid+nanos convention — because
        // a shared `config.tmp` opened with truncate lets two concurrent writers
        // interleave into one file and produce malformed JSON, which the loader
        // then "repairs" by discarding every token. Threads matter as much as
        // processes here: the MCP server serves tool calls concurrently.
        let pid = std::process::id();
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let tmp_file_path = self
            .root_config_path
            .with_extension(format!("tmp.{pid}-{nanos}"));
        let mut options = File::options();
        options.create(true).write(true).truncate(true);
        // Set the mode at creation so the tokens are never briefly readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let tmp_file = options.open(&tmp_file_path)?;
        // An existing tmp file keeps its old mode, so enforce it either way.
        secure_config_file(&tmp_file_path)?;
        serde_json::to_writer_pretty(&tmp_file, value)?;
        tmp_file.sync_all()?;

        // Rename to the final destination. `rename_replacing` is atomic on both
        // Unix and Windows.
        crate::util::rename_replacing(tmp_file_path.as_path(), &self.root_config_path)?;

        Ok(())
    }

    /// Resolves env-var-based project targeting. Returns:
    /// - `Ok(Some(...))` if RAILWAY_PROJECT_ID is set (with optional environment)
    /// - `Ok(None)` if neither env var is set (fall through to local config)
    /// - `Err(...)` if RAILWAY_ENVIRONMENT_ID is set without RAILWAY_PROJECT_ID
    fn resolve_env_var_project() -> Result<Option<ResolvedEnvVarProject>> {
        let project_id = Self::get_railway_project_id();
        let environment_id = Self::get_railway_environment_id();

        match (project_id, environment_id) {
            (Some(project_id), env_id) => Ok(Some(ResolvedEnvVarProject {
                project_id,
                environment_id: env_id,
            })),
            (None, Some(_)) => {
                bail!("RAILWAY_ENVIRONMENT_ID cannot be set without RAILWAY_PROJECT_ID.")
            }
            (None, None) => Ok(None),
        }
    }
}

#[derive(Debug)]
struct ResolvedEnvVarProject {
    project_id: String,
    environment_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Env var tests must run sequentially to avoid races.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env_vars<F, R>(vars: &[(&str, Option<&str>)], f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous: Vec<(&str, Option<String>)> = vars
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect();

        // SAFETY: tests run sequentially under ENV_LOCK, so no concurrent mutation.
        unsafe {
            for (key, val) in vars {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
        let result = f();
        unsafe {
            for (key, val) in previous {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
        result
    }

    fn empty_configs() -> Configs {
        Configs {
            root_config_path: std::path::PathBuf::new(),
            root_config: RailwayConfig::default(),
        }
    }

    #[test]
    fn env_var_project_id_only_returns_none_environment() {
        let result = with_env_vars(
            &[
                ("RAILWAY_PROJECT_ID", Some("proj-123")),
                ("RAILWAY_ENVIRONMENT_ID", None),
            ],
            Configs::resolve_env_var_project,
        );
        let resolved = result.unwrap().expect("should return Some");
        assert_eq!(resolved.project_id, "proj-123");
        assert!(resolved.environment_id.is_none());
    }

    #[test]
    fn env_var_environment_id_without_project_id_is_rejected() {
        let result = with_env_vars(
            &[
                ("RAILWAY_PROJECT_ID", None),
                ("RAILWAY_ENVIRONMENT_ID", Some("env-456")),
            ],
            Configs::resolve_env_var_project,
        );
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("RAILWAY_ENVIRONMENT_ID cannot be set without RAILWAY_PROJECT_ID"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn auth_credentials_accept_project_token() {
        let configs = empty_configs();
        let has_credentials = with_env_vars(
            &[
                ("RAILWAY_TOKEN", Some("project-token")),
                ("RAILWAY_API_TOKEN", None),
            ],
            || configs.has_auth_credentials(),
        );

        assert!(has_credentials);
    }
}

/// How long to wait for the config lock before giving up and proceeding without
/// it. Kept short so a stale lock can never wedge the CLI for long.
const CONFIG_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Poll interval while waiting for the config lock.
const CONFIG_LOCK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// RAII guard around an exclusive advisory lock on the config lockfile.
/// Releasing happens on drop, covering all error paths.
pub(crate) struct ConfigLock {
    file: Option<File>,
}

impl ConfigLock {
    /// Acquire the lock guarding `config_path`. The lockfile is a sibling of the
    /// config (never the config itself, so locking cannot interfere with the
    /// atomic rename). Failure to lock is non-fatal: proceeding unlocked is
    /// strictly better than wedging the CLI, and the credential merge in `write`
    /// still narrows the race to microseconds.
    fn acquire(config_path: &std::path::Path) -> Self {
        let lock_path = config_path.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            if create_dir_all(parent).is_err() {
                return Self { file: None };
            }
        }
        let Ok(file) = File::create(&lock_path) else {
            return Self { file: None };
        };

        use fs2::FileExt;
        let deadline = std::time::Instant::now() + CONFIG_LOCK_TIMEOUT;
        loop {
            if file.try_lock_exclusive().is_ok() {
                return Self { file: Some(file) };
            }
            if std::time::Instant::now() >= deadline {
                eprintln!(
                    "{}",
                    "Warning: timed out waiting for config lock; proceeding without it".yellow()
                );
                return Self { file: None };
            }
            std::thread::sleep(CONFIG_LOCK_POLL_INTERVAL);
        }
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            use fs2::FileExt;
            let _ = FileExt::unlock(&file);
        }
    }
}

/// `~/.railway` holds OAuth tokens and resolved project secrets, so it is kept
/// owner-only rather than left to the ambient umask. Existing directories are
/// repaired, matching what the SSH config writer already does for `~/.ssh`.
#[cfg(unix)]
pub fn secure_config_dir(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("Failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
pub fn secure_config_dir(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

/// Owner-only mode for a file that carries credentials or resolved secrets.
#[cfg(unix)]
pub fn secure_config_file(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("Failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
pub fn secure_config_file(_path: &std::path::Path) -> Result<()> {
    Ok(())
}
