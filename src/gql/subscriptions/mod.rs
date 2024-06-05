use graphql_client::GraphQLQuery;
type DateTime = chrono::DateTime<chrono::Utc>;

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
    query_path = "src/gql/subscriptions/strings/DeploymentEvents.graphql",
    response_derives = "Debug, Serialize, Clone, PartialEq, Eq"
)]
pub struct DeploymentEvents;
