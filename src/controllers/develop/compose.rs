use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::ports::generate_port;
use crate::controllers::config::ServiceInstance;

#[derive(Debug, Clone)]
pub enum PortType {
    Http,
    Tcp,
}

#[derive(Debug, Clone)]
pub struct PortInfo {
    pub internal: i64,
    pub external: u16,
    pub public_port: u16,
    pub port_type: PortType,
}

#[derive(Debug, Serialize)]
pub struct DockerComposeFile {
    pub services: BTreeMap<String, DockerComposeService>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub networks: Option<DockerComposeNetworks>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub volumes: BTreeMap<String, DockerComposeVolume>,
}

#[derive(Debug, Serialize)]
pub struct DockerComposeVolume {}

#[derive(Debug, Serialize)]
pub struct DockerComposeNetworks {
    pub railway: DockerComposeNetwork,
}

#[derive(Debug, Serialize)]
pub struct DockerComposeNetwork {
    pub driver: String,
}

#[derive(Debug, Serialize)]
pub struct DockerComposeService {
    pub image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ComposeServiceStatus {
    #[serde(rename = "Service")]
    pub service: String,
    #[serde(rename = "State")]
    pub state: String,
    #[serde(rename = "Health")]
    pub health: String,
    #[serde(rename = "ExitCode")]
    pub exit_code: i32,
}

pub fn volume_name(environment_id: &str, volume_id: &str) -> String {
    format!("railway_{}_{}", &environment_id[..8], &volume_id[..8])
}

pub fn build_port_infos(service_id: &str, svc: &ServiceInstance) -> Vec<PortInfo> {
    let mut port_infos = Vec::new();
    if let Some(networking) = &svc.networking {
        for config in networking.service_domains.values().flatten() {
            if let Some(port) = config.port {
                if !port_infos.iter().any(|p: &PortInfo| p.internal == port) {
                    let private_port = generate_port(service_id, port);
                    let public_port = generate_port(service_id, port + 10000);
                    port_infos.push(PortInfo {
                        internal: port,
                        external: private_port,
                        public_port,
                        port_type: PortType::Http,
                    });
                }
            }
        }
        for port_str in networking.tcp_proxies.keys() {
            if let Ok(port) = port_str.parse::<i64>() {
                if !port_infos.iter().any(|p| p.internal == port) {
                    let ext_port = generate_port(service_id, port);
                    port_infos.push(PortInfo {
                        internal: port,
                        external: ext_port,
                        public_port: ext_port,
                        port_type: PortType::Tcp,
                    });
                }
            }
        }
    }
    port_infos
}

pub fn build_slug_port_mapping(service_id: &str, svc: &ServiceInstance) -> HashMap<i64, u16> {
    let mut mapping = HashMap::new();
    if let Some(networking) = &svc.networking {
        for config in networking.service_domains.values().flatten() {
            if let Some(port) = config.port {
                mapping
                    .entry(port)
                    .or_insert_with(|| generate_port(service_id, port));
            }
        }
        for port_str in networking.tcp_proxies.keys() {
            if let Ok(port) = port_str.parse::<i64>() {
                mapping
                    .entry(port)
                    .or_insert_with(|| generate_port(service_id, port));
            }
        }
    }
    mapping
}
