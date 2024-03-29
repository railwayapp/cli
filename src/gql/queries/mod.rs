use graphql_client::GraphQLQuery;
use serde::{Deserialize, Serialize};

type DateTime = chrono::DateTime<chrono::Utc>;
type ServiceVariables = std::collections::BTreeMap<String, String>;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TemplateServiceConfig {
    pub name: String,
    pub icon: Option<String>,
    pub source: TemplateServiceConfigIcon,
    pub variables: Vec<TemplateServiceVariables>,
    pub domains: Vec<TemplateServiceDomainConfig>,
    pub tcp_proxies: Option<Vec<TemplateServiceTcpProxy>>,
    // buildConfig is irrelevant
    pub deploy_config: Option<TemplateServiceDeployConfig>,
    pub volumes: Option<Vec<TemplateServiceVolumeConfig>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TemplateServiceDeployConfig {
    pub start_command: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TemplateServiceVolumeConfig {
    pub mount_path: String,
    pub name: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TemplateServiceTcpProxy {
    pub application_port: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TemplateServiceDomainConfig {
    pub has_service_domain: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TemplateServiceVariables {
    pub name: String,
    pub description: Option<String>,
    pub default_value: Option<String>,
    pub is_optional: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TemplateServiceConfigIcon {
    pub image: String,
}

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/queries/strings/Project.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct Project;
pub type RailwayProject = project::ProjectProject;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/queries/strings/Projects.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct Projects;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/queries/strings/UserMeta.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct UserMeta;
pub type RailwayUser = user_meta::UserMetaMe;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/queries/strings/TwoFactorInfo.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct TwoFactorInfo;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/queries/strings/UserProjects.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct UserProjects;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/queries/strings/VariablesForServiceDeployment.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct VariablesForServiceDeployment;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/queries/strings/Deployments.graphql",
    response_derives = "Debug, Serialize, Clone, PartialEq"
)]
pub struct Deployments;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/queries/strings/Deployment.graphql",
    response_derives = "Debug, Serialize, Clone, PartialEq"
)]
pub struct Deployment;
pub type RailwayDeployment = deployment::DeploymentDeployment;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/queries/strings/BuildLogs.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct BuildLogs;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/queries/strings/Domains.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct Domains;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/queries/strings/ProjectToken.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ProjectToken;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/queries/strings/TemplateDetail.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct TemplateDetail;
