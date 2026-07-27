use graphql_client::GraphQLQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/subscriptions/strings/BuildLogs.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct BuildLogs;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/subscriptions/strings/DeploymentLogs.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct DeploymentLogs;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/subscriptions/strings/HttpLogs.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct HttpLogs;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/subscriptions/strings/NetworkFlowLogs.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct NetworkFlowLogs;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/subscriptions/strings/DnsQueryLogs.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct DnsQueryLogs;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/subscriptions/strings/Deployment.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct Deployment;
