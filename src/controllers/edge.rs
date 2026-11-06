//! Helpers shared by the edge-config commands (`cdn`, `waf`, `edge-rules`):
//! the public-domain availability check and the common output shapes.

use std::fmt;

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;

use crate::{
    client::post_graphql,
    controllers::project::ServiceContext,
    gql::queries::{self, domains::DomainsDomains},
};

pub const FIELD_LABEL_WIDTH: usize = 22;

pub async fn fetch_domains(ctx: &ServiceContext) -> Result<DomainsDomains> {
    let response = post_graphql::<queries::Domains, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        queries::domains::Variables {
            project_id: ctx.project_id.clone(),
            environment_id: ctx.environment_id.clone(),
            service_id: ctx.service_id.clone(),
        },
    )
    .await?;
    Ok(response.domains)
}

/// Edge config exists only once a service has an applied public domain.
pub fn has_public_domain(domains: &DomainsDomains) -> bool {
    domains
        .service_domains
        .iter()
        .any(|domain| is_applied_domain_sync_status(&domain.sync_status))
        || domains
            .custom_domains
            .iter()
            .any(|domain| is_applied_domain_sync_status(&domain.sync_status))
}

pub fn is_applied_domain_sync_status<T: fmt::Debug>(sync_status: &T) -> bool {
    matches!(enum_name(sync_status).as_str(), "ACTIVE" | "UNSPECIFIED")
}

pub fn enum_name<T: fmt::Debug>(value: &T) -> String {
    format!("{value:?}")
}

pub fn print_field(label: &str, value: &dyn fmt::Display) {
    let padded = format!("{label:<FIELD_LABEL_WIDTH$}");
    println!("{} {value}", padded.dimmed());
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRef {
    pub id: String,
    pub name: String,
}

impl ResourceRef {
    pub fn service(ctx: &ServiceContext) -> Self {
        Self {
            id: ctx.service_id.clone(),
            name: ctx.service_name.clone(),
        }
    }

    pub fn environment(ctx: &ServiceContext) -> Self {
        Self {
            id: ctx.environment_id.clone(),
            name: ctx.environment_name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_domain_availability_requires_applied_sync_status() {
        let empty = domains(Vec::new());
        assert!(!has_public_domain(&empty));

        let creating = domains(vec![queries::domains::ServiceDomainSyncStatus::CREATING]);
        assert!(!has_public_domain(&creating));

        let deleting = domains(vec![queries::domains::ServiceDomainSyncStatus::DELETING]);
        assert!(!has_public_domain(&deleting));

        let active = domains(vec![queries::domains::ServiceDomainSyncStatus::ACTIVE]);
        assert!(has_public_domain(&active));

        let unspecified = domains(vec![queries::domains::ServiceDomainSyncStatus::UNSPECIFIED]);
        assert!(has_public_domain(&unspecified));
    }

    fn domains(sync_statuses: Vec<queries::domains::ServiceDomainSyncStatus>) -> DomainsDomains {
        DomainsDomains {
            service_domains: sync_statuses
                .into_iter()
                .enumerate()
                .map(
                    |(index, sync_status)| queries::domains::DomainsDomainsServiceDomains {
                        id: format!("sd_{index}"),
                        domain: format!("web-{index}.up.railway.app"),
                        suffix: Some("up.railway.app".to_string()),
                        environment_id: "env_123".to_string(),
                        service_id: "svc_123".to_string(),
                        target_port: None,
                        sync_status,
                        created_at: None,
                        updated_at: None,
                    },
                )
                .collect(),
            custom_domains: Vec::new(),
        }
    }
}
