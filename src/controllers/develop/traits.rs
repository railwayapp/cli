#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Output;

use anyhow::Result;
use async_trait::async_trait;

use crate::controllers::config::EnvironmentConfig;

/// Abstracts Railway API calls for environment config and variables.
/// Enables testing with mock data instead of real API calls.
#[async_trait]
pub trait EnvironmentDataProvider: Send + Sync {
    /// Fetch the environment configuration (services, networking, variables)
    async fn fetch_environment_config(&self, environment_id: &str) -> Result<EnvironmentConfig>;

    /// Fetch resolved variables for a specific service deployment
    async fn fetch_service_variables(
        &self,
        project_id: &str,
        environment_id: &str,
        service_id: &str,
    ) -> Result<BTreeMap<String, String>>;
}

/// Abstracts external command execution (docker, mkcert, etc).
/// Enables testing without actually running commands.
#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// Run a command and return its output
    async fn run(&self, program: &str, args: &[&str]) -> Result<Output>;

    /// Run a command in a specific working directory
    async fn run_in_dir(&self, program: &str, args: &[&str], cwd: &Path) -> Result<Output>;

    /// Run a command synchronously (for simple checks)
    fn run_sync(&self, program: &str, args: &[&str]) -> Result<Output>;
}

/// Output from a command execution
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

impl From<Output> for CommandOutput {
    fn from(output: Output) -> Self {
        Self {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code(),
        }
    }
}

// --- Real implementations ---

use anyhow::Context;
use reqwest::Client;
use std::process::Command;
use tokio::process::Command as TokioCommand;

use crate::{client::post_graphql, config::Configs, gql::queries};

/// Real implementation that fetches data from Railway API
pub struct RealEnvironmentDataProvider {
    client: Client,
    configs: Configs,
}

impl RealEnvironmentDataProvider {
    pub fn new(client: Client, configs: Configs) -> Self {
        Self { client, configs }
    }
}

#[async_trait]
impl EnvironmentDataProvider for RealEnvironmentDataProvider {
    async fn fetch_environment_config(&self, environment_id: &str) -> Result<EnvironmentConfig> {
        let vars = queries::get_environment_config::Variables {
            id: environment_id.to_string(),
            decrypt_variables: Some(false),
        };

        let data = post_graphql::<queries::GetEnvironmentConfig, _>(
            &self.client,
            self.configs.get_backboard(),
            vars,
        )
        .await?;

        let config: EnvironmentConfig = serde_json::from_value(data.environment.config)
            .context("Failed to parse environment config")?;

        Ok(config)
    }

    async fn fetch_service_variables(
        &self,
        project_id: &str,
        environment_id: &str,
        service_id: &str,
    ) -> Result<BTreeMap<String, String>> {
        let vars = queries::variables_for_service_deployment::Variables {
            project_id: project_id.to_string(),
            environment_id: environment_id.to_string(),
            service_id: service_id.to_string(),
        };

        let response = post_graphql::<queries::VariablesForServiceDeployment, _>(
            &self.client,
            self.configs.get_backboard(),
            vars,
        )
        .await?;

        let variables = response
            .variables_for_service_deployment
            .into_iter()
            .filter_map(|(k, v)| v.map(|val| (k, val)))
            .collect();

        Ok(variables)
    }
}

/// Real implementation that runs actual commands
pub struct RealCommandRunner;

impl RealCommandRunner {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RealCommandRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CommandRunner for RealCommandRunner {
    async fn run(&self, program: &str, args: &[&str]) -> Result<Output> {
        TokioCommand::new(program)
            .args(args)
            .output()
            .await
            .with_context(|| format!("Failed to run {}", program))
    }

    async fn run_in_dir(&self, program: &str, args: &[&str], cwd: &Path) -> Result<Output> {
        TokioCommand::new(program)
            .args(args)
            .current_dir(cwd)
            .output()
            .await
            .with_context(|| format!("Failed to run {} in {:?}", program, cwd))
    }

    fn run_sync(&self, program: &str, args: &[&str]) -> Result<Output> {
        Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run {}", program))
    }
}

// --- Mock implementations for testing ---

#[cfg(test)]
pub mod mocks {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Mock implementation of EnvironmentDataProvider for testing
    pub struct MockEnvironmentDataProvider {
        pub configs: HashMap<String, EnvironmentConfig>,
        pub variables: HashMap<(String, String, String), BTreeMap<String, String>>,
    }

    impl MockEnvironmentDataProvider {
        pub fn new() -> Self {
            Self {
                configs: HashMap::new(),
                variables: HashMap::new(),
            }
        }

        pub fn with_config(mut self, env_id: &str, config: EnvironmentConfig) -> Self {
            self.configs.insert(env_id.to_string(), config);
            self
        }

        pub fn with_variables(
            mut self,
            project_id: &str,
            env_id: &str,
            service_id: &str,
            vars: BTreeMap<String, String>,
        ) -> Self {
            self.variables.insert(
                (
                    project_id.to_string(),
                    env_id.to_string(),
                    service_id.to_string(),
                ),
                vars,
            );
            self
        }
    }

    impl Default for MockEnvironmentDataProvider {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl EnvironmentDataProvider for MockEnvironmentDataProvider {
        async fn fetch_environment_config(
            &self,
            environment_id: &str,
        ) -> Result<EnvironmentConfig> {
            self.configs
                .get(environment_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Environment not found: {}", environment_id))
        }

        async fn fetch_service_variables(
            &self,
            project_id: &str,
            environment_id: &str,
            service_id: &str,
        ) -> Result<BTreeMap<String, String>> {
            let key = (
                project_id.to_string(),
                environment_id.to_string(),
                service_id.to_string(),
            );
            self.variables
                .get(&key)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Variables not found for service: {}", service_id))
        }
    }

    /// Mock implementation of CommandRunner for testing
    pub struct MockCommandRunner {
        pub responses: Mutex<Vec<Output>>,
        pub calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl MockCommandRunner {
        pub fn new() -> Self {
            Self {
                responses: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
            }
        }

        pub fn with_response(self, output: Output) -> Self {
            self.responses.lock().unwrap().push(output);
            self
        }

        pub fn success_response() -> Output {
            Output {
                status: std::process::ExitStatus::default(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }
        }

        pub fn get_calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Default for MockCommandRunner {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl CommandRunner for MockCommandRunner {
        async fn run(&self, program: &str, args: &[&str]) -> Result<Output> {
            self.calls.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            ));

            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                Ok(Self::success_response())
            } else {
                Ok(responses.remove(0))
            }
        }

        async fn run_in_dir(&self, program: &str, args: &[&str], _cwd: &Path) -> Result<Output> {
            self.run(program, args).await
        }

        fn run_sync(&self, program: &str, args: &[&str]) -> Result<Output> {
            self.calls.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            ));

            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                Ok(Self::success_response())
            } else {
                Ok(responses.remove(0))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mocks::*;
    use super::*;

    #[tokio::test]
    async fn test_mock_environment_provider() {
        let mut vars = BTreeMap::new();
        vars.insert("DATABASE_URL".to_string(), "postgres://...".to_string());

        let provider = MockEnvironmentDataProvider::new()
            .with_config("env-1", EnvironmentConfig::default())
            .with_variables("proj-1", "env-1", "svc-1", vars);

        let config = provider.fetch_environment_config("env-1").await.unwrap();
        assert!(config.services.is_empty());

        let variables = provider
            .fetch_service_variables("proj-1", "env-1", "svc-1")
            .await
            .unwrap();
        assert_eq!(
            variables.get("DATABASE_URL"),
            Some(&"postgres://...".to_string())
        );
    }

    #[tokio::test]
    async fn test_mock_command_runner() {
        let runner = MockCommandRunner::new();

        let output = runner.run("echo", &["hello"]).await.unwrap();
        assert!(output.status.success());

        let calls = runner.get_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "echo");
        assert_eq!(calls[0].1, vec!["hello"]);
    }
}
