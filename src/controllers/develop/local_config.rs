use std::{collections::HashMap, fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::ports::get_develop_dir;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct LocalDevConfig {
    pub version: u32,
    pub services: HashMap<String, CodeServiceConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CodeServiceConfig {
    pub command: String,
    pub directory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl LocalDevConfig {
    pub fn path(project_id: &str) -> PathBuf {
        get_develop_dir(project_id).join("local-dev.json")
    }

    pub fn load(project_id: &str) -> Result<Self> {
        let path = Self::path(project_id);
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))
    }

    pub fn save(&self, project_id: &str) -> Result<()> {
        let path = Self::path(project_id);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }

        let tmp_path = path.with_extension("tmp");
        let content = serde_json::to_string_pretty(self)?;
        fs::write(&tmp_path, content)
            .with_context(|| format!("Failed to write {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &path).with_context(|| {
            format!(
                "Failed to rename {} to {}",
                tmp_path.display(),
                path.display()
            )
        })?;

        Ok(())
    }

    pub fn get_service(&self, service_id: &str) -> Option<&CodeServiceConfig> {
        self.services.get(service_id)
    }

    pub fn set_service(&mut self, service_id: String, config: CodeServiceConfig) {
        self.services.insert(service_id, config);
    }

    pub fn remove_service(&mut self, service_id: &str) -> Option<CodeServiceConfig> {
        self.services.remove(service_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_operations() {
        let mut config = LocalDevConfig::default();
        assert!(config.get_service("svc-123").is_none());

        config.set_service(
            "svc-123".to_string(),
            CodeServiceConfig {
                command: "npm start".to_string(),
                directory: "/app".to_string(),
                port: Some(3000),
            },
        );

        assert!(config.get_service("svc-123").is_some());
        assert_eq!(config.get_service("svc-123").unwrap().command, "npm start");

        config.remove_service("svc-123");
        assert!(config.get_service("svc-123").is_none());
    }
}
