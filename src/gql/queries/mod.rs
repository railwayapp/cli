use graphql_client::GraphQLQuery;

type DateTime = chrono::DateTime<chrono::Utc>;
type BigInt = i64;
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

impl std::fmt::Display
    for environment_instances::EnvironmentInstancesEnvironmentServiceInstancesEdges
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.node.service_name)
    }
}

impl std::fmt::Display
    for environment_instances::EnvironmentInstancesEnvironmentServiceInstancesEdgesNode
{
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
    query_path = "src/gql/queries/strings/HttpLogs.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct HttpLogs;

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
    query_path = "src/gql/queries/strings/PrivateNetworks.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct PrivateNetworks;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/PrivateNetworkEndpoint.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct PrivateNetworkEndpoint;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/PrivateNetworkEndpointNameAvailable.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct PrivateNetworkEndpointNameAvailable;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/ProjectToken.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ProjectToken;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/BucketInstanceDetails.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct BucketInstanceDetails;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/BucketS3Credentials.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct BucketS3Credentials;

pub type SerializedTemplateConfig = serde_json::Value;
pub type EnvironmentConfig = serde_json::Value;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/Template.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct Template;

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
    query_path = "src/gql/queries/strings/WorkspaceTemplates.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct WorkspaceTemplates;

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
    query_path = "src/gql/queries/strings/EnvironmentConfig.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct GetEnvironmentConfig;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/Environments.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct Environments;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/EnvironmentInstances.graphql",
    response_derives = "Debug, Serialize, Clone, PartialEq"
)]
pub struct EnvironmentInstances;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/WorkflowStatus.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct WorkflowStatus;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/Metrics.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct Metrics;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/HttpMetricsByStatus.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct HttpMetricsByStatus;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/HttpDurationMetrics.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct HttpDurationMetrics;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/SshPublicKeys.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct SshPublicKeys;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/Sandbox.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct Sandbox;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/Sandboxes.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct Sandboxes;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/SandboxTemplateBuild.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct SandboxTemplateBuild;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/SandboxCheckpoints.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct SandboxCheckpoints;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/TemplateSearch.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct TemplateSearch;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/GitHubSshKeys.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct GitHubSshKeys;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/queries/strings/ServiceInstance.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ServiceInstance;

type SubscriptionDeploymentStatus = super::subscriptions::deployment::DeploymentStatus;
impl From<environment_instances::DeploymentStatus> for SubscriptionDeploymentStatus {
    fn from(value: environment_instances::DeploymentStatus) -> Self {
        match value {
            environment_instances::DeploymentStatus::BUILDING => {
                SubscriptionDeploymentStatus::BUILDING
            }
            environment_instances::DeploymentStatus::CRASHED => {
                SubscriptionDeploymentStatus::CRASHED
            }
            environment_instances::DeploymentStatus::DEPLOYING => {
                SubscriptionDeploymentStatus::DEPLOYING
            }
            environment_instances::DeploymentStatus::FAILED => SubscriptionDeploymentStatus::FAILED,
            environment_instances::DeploymentStatus::INITIALIZING => {
                SubscriptionDeploymentStatus::INITIALIZING
            }
            environment_instances::DeploymentStatus::NEEDS_APPROVAL => {
                SubscriptionDeploymentStatus::NEEDS_APPROVAL
            }
            environment_instances::DeploymentStatus::QUEUED => SubscriptionDeploymentStatus::QUEUED,
            environment_instances::DeploymentStatus::REMOVED => {
                SubscriptionDeploymentStatus::REMOVED
            }
            environment_instances::DeploymentStatus::REMOVING => {
                SubscriptionDeploymentStatus::REMOVING
            }
            environment_instances::DeploymentStatus::SKIPPED => {
                SubscriptionDeploymentStatus::SKIPPED
            }
            environment_instances::DeploymentStatus::SLEEPING => {
                SubscriptionDeploymentStatus::SLEEPING
            }
            environment_instances::DeploymentStatus::SUCCESS => {
                SubscriptionDeploymentStatus::SUCCESS
            }
            environment_instances::DeploymentStatus::WAITING => {
                SubscriptionDeploymentStatus::WAITING
            }
            environment_instances::DeploymentStatus::Other(s) => {
                SubscriptionDeploymentStatus::Other(s)
            }
        }
    }
}
