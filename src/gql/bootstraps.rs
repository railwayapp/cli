//! Named cloud-agent bootstraps and their environment's shared default.
use graphql_client::GraphQLQuery;

type DateTime = chrono::DateTime<chrono::Utc>;
#[allow(clippy::upper_case_acronyms)]
type JSON = serde_json::Value;
type AgentBootstrapManifest = serde_json::Value;

macro_rules! operation {
    ($name:ident) => {
        #[derive(GraphQLQuery)]
        #[graphql(
            schema_path = "src/gql/schema.json",
            query_path = "src/gql/queries/strings/AgentBootstraps.graphql",
            response_derives = "Debug, Serialize, Clone"
        )]
        pub struct $name;
    };
}
operation!(AgentBootstraps);
operation!(AgentBootstrap);
operation!(AgentBootstrapDefault);
operation!(AgentBootstrapSave);
operation!(AgentBootstrapSetDefault);
