use crate::{
    controllers::{database::DatabaseType, project::ProjectServiceInstanceEdge},
    gql::queries::{self},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceKind {
    Service,
    Database,
    Function,
    CronJob,
}

pub(crate) fn classify_service_instance(
    service_instance: &ProjectServiceInstanceEdge,
) -> ResourceKind {
    if service_instance.node.cron_schedule.is_some() {
        ResourceKind::CronJob
    } else if is_database_instance(service_instance) {
        ResourceKind::Database
    } else if is_function_service(service_instance) {
        ResourceKind::Function
    } else {
        ResourceKind::Service
    }
}

pub(crate) fn is_function_service(service_instance: &ProjectServiceInstanceEdge) -> bool {
    service_instance.node.source.as_ref().is_some_and(|source| {
        source
            .image
            .as_deref()
            .unwrap_or_default()
            .starts_with("ghcr.io/railwayapp/function")
    })
}

pub(crate) fn is_database_instance(service_instance: &ProjectServiceInstanceEdge) -> bool {
    is_database_service(source_image(service_instance))
}

pub(crate) fn is_database_service(source_image: Option<&str>) -> bool {
    source_image
        .map(|img| img.to_lowercase())
        .is_some_and(|img| {
            img.contains("postgres")
                || img.contains("postgis")
                || img.contains("timescale")
                || img.contains("redis")
                || img.contains("mongo")
                || img.contains("mysql")
                || img.contains("mariadb")
                || img.contains("memcached")
                || img.contains("valkey")
        })
}

pub(crate) fn detect_database_type(source_image: Option<&str>) -> Option<DatabaseType> {
    let image = source_image?.to_ascii_lowercase();
    if image.contains("postgres") || image.contains("postgis") || image.contains("timescale") {
        Some(DatabaseType::PostgreSQL)
    } else if image.contains("redis") || image.contains("valkey") {
        Some(DatabaseType::Redis)
    } else if image.contains("mongo") {
        Some(DatabaseType::MongoDB)
    } else if image.contains("mysql") || image.contains("mariadb") {
        Some(DatabaseType::MySQL)
    } else {
        None
    }
}

pub(crate) fn database_label(
    service_instance: &ProjectServiceInstanceEdge,
) -> Option<&'static str> {
    let image = source_image(service_instance)?.to_lowercase();
    if image.contains("postgres") || image.contains("postgis") || image.contains("timescale") {
        Some("Postgres")
    } else if image.contains("redis") || image.contains("valkey") {
        Some("Redis")
    } else if image.contains("mongo") {
        Some("MongoDB")
    } else if image.contains("mysql") || image.contains("mariadb") {
        Some("MySQL")
    } else if image.contains("memcached") {
        Some("Memcached")
    } else {
        None
    }
}

pub(crate) fn name_mentions(name: &str, label: &str) -> bool {
    let name = name.to_lowercase();
    let label = label.to_lowercase();
    name.contains(&label) || (label == "postgres" && name.contains("postgresql"))
}

pub(crate) fn project_bucket_name(
    project: &queries::RailwayProject,
    bucket_id: &str,
) -> Option<String> {
    project
        .buckets
        .edges
        .iter()
        .find(|edge| edge.node.id == bucket_id)
        .map(|edge| edge.node.name.clone())
}

fn source_image(service_instance: &ProjectServiceInstanceEdge) -> Option<&str> {
    service_instance
        .node
        .source
        .as_ref()
        .and_then(|source| source.image.as_deref())
}
