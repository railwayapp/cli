use graphql_client::GraphQLQuery;
use serde::{Deserialize, Serialize};
type EnvironmentVariables = std::collections::BTreeMap<String, String>;
#[allow(clippy::upper_case_acronyms)] // graphql client expects a type called JSON
type JSON = serde_json::Value;
use chrono::{DateTime as DateTimeType, Utc};

pub type DateTime = DateTimeType<Utc>;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TemplateVolume {
    pub mount_path: String,
    pub name: Option<String>,
}

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/DeploymentRemove.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct DeploymentRemove;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/LoginSessionConsume.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct LoginSessionConsume;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/LoginSessionCreate.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct LoginSessionCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/ProjectCreate.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ProjectCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/ServiceDomainCreate.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ServiceDomainCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/ValidateTwoFactor.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ValidateTwoFactor;

pub type SerializedTemplateConfig = serde_json::Value;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/TemplateDeploy.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct TemplateDeploy;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/VolumeCreate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct VolumeCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/VolumeDelete.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct VolumeDelete;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/VolumeMountPathUpdate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct VolumeMountPathUpdate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/VolumeNameUpdate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct VolumeNameUpdate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/VolumeDetach.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct VolumeDetach;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/VolumeAttach.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct VolumeAttach;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/DeploymentRedeploy.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct DeploymentRedeploy;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/DeploymentRestart.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct DeploymentRestart;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/VariableCollectionUpsert.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct VariableCollectionUpsert;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/ServiceCreate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct ServiceCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/CustomDomainCreate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct CustomDomainCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/UpdateRegions.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct UpdateRegions;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/EnvironmentCreate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct EnvironmentCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/ServiceInstanceDeploy.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ServiceInstanceDeploy;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/ServiceDelete.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ServiceDelete;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/EnvironmentDelete.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct EnvironmentDelete;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/FunctionUpdate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct FunctionUpdate;

impl std::fmt::Display for custom_domain_create::DNSRecordType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DNS_RECORD_TYPE_CNAME => write!(f, "CNAME"),
            Self::DNS_RECORD_TYPE_A => write!(f, "A"),
            Self::DNS_RECORD_TYPE_NS => write!(f, "NS"),
            Self::DNS_RECORD_TYPE_UNSPECIFIED => write!(f, "UNSPECIFIED"),
            Self::UNRECOGNIZED => write!(f, "UNRECOGNIZED"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}
