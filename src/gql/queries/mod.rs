use graphql_client::GraphQLQuery;

type DateTime = chrono::DateTime<chrono::Utc>;
type EnvironmentVariables = std::collections::BTreeMap<String, Option<String>>;
//type DeploymentMeta = std::collections::BTreeMap<String, serde_json::Value>;
type DeploymentMeta = serde_json::Value;

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

impl std::fmt::Display for project::ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.node.service_name)
    }
}

impl std::fmt::Display for project::ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdgesNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.service_name)
    }
}

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
