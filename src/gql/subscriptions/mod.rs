use graphql_client::GraphQLQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/subscriptions/strings/BuildLogs.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct BuildLogs;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/subscriptions/strings/DeploymentLogs.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct DeploymentLogs;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.graphql",
    query_path = "src/gql/subscriptions/strings/Deployment.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct Deployment;
