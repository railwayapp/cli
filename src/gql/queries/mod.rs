use graphql_client::GraphQLQuery;
use serde::{Deserialize, Serialize};

type DateTime = chrono::DateTime<chrono::Utc>;
type EnvironmentVariables = std::collections::BTreeMap<String, Option<String>>;
//type DeploymentMeta = std::collections::BTreeMap<String, serde_json::Value>;
type DeploymentMeta = serde_json::Value;

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
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/Project.graphql",
    response_derives = "Debug, Serialize, Clone, PartialEq"
)]
pub struct Project;
pub type RailwayProject = project::ProjectProject;

impl std::fmt::Display for project::ProjectProjectServicesEdges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.node.name)
    }
}

impl std::fmt::Display for project::ProjectProjectServicesEdgesNodeServiceInstancesEdges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.node.service_name)
    }
}

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/Projects.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct Projects;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/UserMeta.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct UserMeta;
pub type RailwayUser = user_meta::UserMetaMe;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/TwoFactorInfo.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct TwoFactorInfo;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/UserProjects.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct UserProjects;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/VariablesForServiceDeployment.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct VariablesForServiceDeployment;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/Deployments.graphql",
    response_derives = "Debug, Serialize, Clone, PartialEq"
)]
pub struct Deployments;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/BuildLogs.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct BuildLogs;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/DeploymentLogs.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct DeploymentLogs;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/Domains.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct Domains;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/ProjectToken.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ProjectToken;

pub type SerializedTemplateConfig = serde_json::Value;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/TemplateDetail.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct TemplateDetail;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/GitHubRepos.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct GitHubRepos;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/CustomDomainAvailable.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct CustomDomainAvailable;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/Regions.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct Regions;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/LatestFunctionVersion.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct LatestFunctionVersion;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/LatestDeployment.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct LatestDeployment;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/EnvironmentStagedChanges.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct EnvironmentStagedChanges;

type SubscriptionDeploymentStatus = super::subscriptions::deployment::DeploymentStatus;
impl From<project::DeploymentStatus> for SubscriptionDeploymentStatus {
    fn from(value: project::DeploymentStatus) -> Self {
        match value {
            project::DeploymentStatus::BUILDING => SubscriptionDeploymentStatus::BUILDING,
            project::DeploymentStatus::CRASHED => SubscriptionDeploymentStatus::CRASHED,
            project::DeploymentStatus::DEPLOYING => SubscriptionDeploymentStatus::DEPLOYING,
            project::DeploymentStatus::FAILED => SubscriptionDeploymentStatus::FAILED,
            project::DeploymentStatus::INITIALIZING => SubscriptionDeploymentStatus::INITIALIZING,
            project::DeploymentStatus::NEEDS_APPROVAL => {
                SubscriptionDeploymentStatus::NEEDS_APPROVAL
            }
            project::DeploymentStatus::QUEUED => SubscriptionDeploymentStatus::QUEUED,
            project::DeploymentStatus::REMOVED => SubscriptionDeploymentStatus::REMOVED,
            project::DeploymentStatus::REMOVING => SubscriptionDeploymentStatus::REMOVING,
            project::DeploymentStatus::SKIPPED => SubscriptionDeploymentStatus::SKIPPED,
            project::DeploymentStatus::SLEEPING => SubscriptionDeploymentStatus::SLEEPING,
            project::DeploymentStatus::SUCCESS => SubscriptionDeploymentStatus::SUCCESS,
            project::DeploymentStatus::WAITING => SubscriptionDeploymentStatus::WAITING,
            project::DeploymentStatus::Other(s) => SubscriptionDeploymentStatus::Other(s),
        }
    }
}
