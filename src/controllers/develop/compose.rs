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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controllers::config::{DomainConfig, ServiceInstance, ServiceNetworking};

    #[test]
    fn test_volume_name() {
        assert_eq!(
            volume_name("env-12345678-xxxx", "vol-abcdefgh-yyyy"),
            "railway_env-1234_vol-abcd"
        );
    }

    #[test]
    fn test_build_port_infos_with_http_domain() {
        let svc = ServiceInstance {
            networking: Some(ServiceNetworking {
                service_domains: BTreeMap::from([(
                    "example.up.railway.app".to_string(),
                    Some(DomainConfig { port: Some(8080) }),
                )]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let ports = build_port_infos("svc-123", &svc);
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].internal, 8080);
        assert!(matches!(ports[0].port_type, PortType::Http));
    }

    #[test]
    fn test_build_port_infos_with_tcp_proxy() {
        let svc = ServiceInstance {
            networking: Some(ServiceNetworking {
                tcp_proxies: BTreeMap::from([("6379".to_string(), None)]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let ports = build_port_infos("redis-svc", &svc);
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].internal, 6379);
        assert!(matches!(ports[0].port_type, PortType::Tcp));
    }

    #[test]
    fn test_build_port_infos_deduplicates() {
        let svc = ServiceInstance {
            networking: Some(ServiceNetworking {
                service_domains: BTreeMap::from([
                    (
                        "a.railway.app".to_string(),
                        Some(DomainConfig { port: Some(3000) }),
                    ),
                    (
                        "b.railway.app".to_string(),
                        Some(DomainConfig { port: Some(3000) }),
                    ),
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let ports = build_port_infos("svc", &svc);
        assert_eq!(ports.len(), 1);
    }

    #[test]
    fn test_build_slug_port_mapping() {
        let svc = ServiceInstance {
            networking: Some(ServiceNetworking {
                service_domains: BTreeMap::from([(
                    "example.railway.app".to_string(),
                    Some(DomainConfig { port: Some(8080) }),
                )]),
                tcp_proxies: BTreeMap::from([("5432".to_string(), None)]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mapping = build_slug_port_mapping("svc-123", &svc);
        assert!(mapping.contains_key(&8080));
        assert!(mapping.contains_key(&5432));
    }
}
