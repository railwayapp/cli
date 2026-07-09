use graphql_client::GraphQLQuery;
type EnvironmentVariables = std::collections::BTreeMap<String, String>;
#[allow(clippy::upper_case_acronyms)] // graphql client expects a type called JSON
type JSON = serde_json::Value;
use chrono::{DateTime as DateTimeType, Utc};

use crate::controllers;

pub type DateTime = DateTimeType<Utc>;
type EnvironmentConfig = controllers::config::EnvironmentConfig;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/CliEventTrack.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
#[allow(dead_code)]
pub struct CliEventTrack;

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
    query_path = "src/gql/mutations/strings/ProjectCreate.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ProjectCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/ProjectScheduleDelete.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ProjectScheduleDelete;

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
    query_path = "src/gql/mutations/strings/ServiceDomainUpdate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct ServiceDomainUpdate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/ServiceDomainDelete.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ServiceDomainDelete;

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
    query_path = "src/gql/mutations/strings/TemplateGenerate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct TemplateGenerate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/TemplatePublish.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct TemplatePublish;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/TemplateUnpublish.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct TemplateUnpublish;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/TemplateDelete.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct TemplateDelete;

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
    query_path = "src/gql/mutations/strings/VariableDelete.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct VariableDelete;

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
    query_path = "src/gql/mutations/strings/ServiceConnect.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct ServiceConnect;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/ServiceDisconnect.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ServiceDisconnect;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/EnableServiceCdn.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct EnableServiceCdn;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/UpdateServiceEdgeConfig.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct UpdateServiceEdgeConfig;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/DisableServiceCdn.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct DisableServiceCdn;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/PurgeServiceCache.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct PurgeServiceCache;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/SetServiceUnderAttackMode.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct SetServiceUnderAttackMode;

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
    query_path = "src/gql/mutations/strings/CustomDomainUpdate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct CustomDomainUpdate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/CustomDomainDelete.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct CustomDomainDelete;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/CustomDomainIssueCertificate.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct CustomDomainIssueCertificate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/RailwayDomainUpdate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct RailwayDomainUpdate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/RailwayDomainDnsRecordCreate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct RailwayDomainDnsRecordCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/RailwayDomainDnsRecordUpdate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct RailwayDomainDnsRecordUpdate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/RailwayDomainDnsRecordDelete.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct RailwayDomainDnsRecordDelete;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/RailwayDomainNameserversSet.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct RailwayDomainNameserversSet;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/TcpProxyDelete.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct TcpProxyDelete;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/PrivateNetworkEndpointRename.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct PrivateNetworkEndpointRename;

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
    query_path = "src/gql/mutations/strings/ServiceInstanceDeployLatestCommit.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct ServiceInstanceDeployLatestCommit;

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
    query_path = "src/gql/mutations/strings/BucketCreate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct BucketCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/BucketUpdate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct BucketUpdate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/BucketCredentialsReset.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct BucketCredentialsReset;

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

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/EnvironmentPatchCommit.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct EnvironmentPatchCommit;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/EnvironmentStageChanges.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct EnvironmentStageChanges;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/EgressGatewayAssociationCreate.graphql",
    response_derives = "Debug, Serialize, Clone, PartialEq",
    skip_serializing_none
)]
pub struct EgressGatewayAssociationCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/EgressGatewayAssociationsClear.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct EgressGatewayAssociationsClear;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/ServiceInstanceUpdate.graphql",
    response_derives = "Debug, Serialize, Clone",
    variables_derives = "Default",
    skip_serializing_none
)]
pub struct ServiceInstanceUpdate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/SshPublicKeyCreate.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct SshPublicKeyCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/SshPublicKeyDelete.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct SshPublicKeyDelete;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/SandboxCreate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct SandboxCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/SandboxTemplateBuild.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct SandboxTemplateBuild;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/SandboxCheckpointCreate.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct SandboxCheckpointCreate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/SandboxCheckpointRename.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct SandboxCheckpointRename;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/SandboxCheckpointDelete.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct SandboxCheckpointDelete;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/GenerateShellToken.graphql",
    response_derives = "Debug, Serialize, Clone",
    skip_serializing_none
)]
pub struct GenerateShellToken;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/SandboxDestroy.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
pub struct SandboxDestroy;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.json",
    query_path = "src/gql/mutations/strings/SandboxHeartbeat.graphql",
    response_derives = "Debug, Serialize, Clone"
)]
#[allow(dead_code)]
pub struct SandboxHeartbeat;

impl std::fmt::Display for custom_domain_create::DNSRecordType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DNS_RECORD_TYPE_CNAME => write!(f, "CNAME"),
            Self::DNS_RECORD_TYPE_A => write!(f, "A"),
            Self::DNS_RECORD_TYPE_NS => write!(f, "NS"),
            Self::DNS_RECORD_TYPE_TXT => write!(f, "TXT"),
            Self::DNS_RECORD_TYPE_UNSPECIFIED => write!(f, "UNSPECIFIED"),
            Self::UNRECOGNIZED => write!(f, "UNRECOGNIZED"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}
