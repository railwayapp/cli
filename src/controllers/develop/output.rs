#![allow(dead_code)]

use super::compose::PortInfo;

pub struct ServiceSummary {
    pub name: String,
    pub image: String,
    pub var_count: usize,
    pub ports: Vec<PortInfo>,
    pub volumes: Vec<String>,
}
