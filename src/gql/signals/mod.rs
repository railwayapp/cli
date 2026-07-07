use graphql_client::GraphQLQuery;

type JSON = serde_json::Value;
type BigInt = i64;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/signals/schema.graphql",
    query_path = "src/gql/signals/queries/Signals.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct Signals;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/signals/schema.graphql",
    query_path = "src/gql/signals/mutations/SignalCreate.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct SignalCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/signals/schema.graphql",
    query_path = "src/gql/signals/mutations/SignalRuleSet.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct SignalRuleSet;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/signals/schema.graphql",
    query_path = "src/gql/signals/mutations/SignalRuleUnset.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct SignalRuleUnset;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/signals/schema.graphql",
    query_path = "src/gql/signals/queries/Signal.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct Signal;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/signals/schema.graphql",
    query_path = "src/gql/signals/mutations/SignalDefaultSet.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct SignalDefaultSet;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/signals/schema.graphql",
    query_path = "src/gql/signals/mutations/SignalDelete.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct SignalDelete;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/signals/schema.graphql",
    query_path = "src/gql/signals/mutations/SignalReplace.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct SignalReplace;
