use graphql_client::GraphQLQuery;

type DateTime = chrono::DateTime<chrono::Utc>;
type ServiceVariables = std::collections::BTreeMap<String, String>;

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
    query_path = "src/gql/queries/strings/VariablesForPlugin.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct VariablesForPlugin;

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
