// GENERATED FILE DO NOT EDIT!!!

package gen

type AllContainersRequest struct {
	Limit *int32
	Offset *int32
	GQL ContainerGQL
}

type DeploymentByIdRequest struct {
	ProjectId string
	DeploymentId string
	GQL DeploymentGQL
}

type AllDeploymentsForProjectRequest struct {
	ProjectId string
	GQL DeploymentGQL
}

type AllDeploymentsForEnvironmentRequest struct {
	ProjectId string
	EnvironmentId string
	GQL DeploymentGQL
}

type AllDeploymentsRequest struct {
	Status *DeployStatus
	Limit *int32
	Offset *int32
	GQL DeploymentGQL
}

type GetDeploymentTriggersByProjectRequest struct {
	ProjectId string
	GQL DeploymentTriggerGQL
}

type ElasticIsUpRequest struct {
	ProjectId string
	EnvironmentId string
	PluginId string
}

type ElasticGetIndiciesRequest struct {
	ProjectId string
	EnvironmentId string
	PluginId string
	GQL ElasticIndexGQL
}

type ElasticSearchIndexRequest struct {
	ProjectId string
	EnvironmentId string
	PluginId string
	Search string
	Index *string
	From *int32
	Size *int32
	GQL ElasticSearchResultGQL
}

type EnvironmentByIdRequest struct {
	ProjectId string
	EnvironmentId string
	GQL EnvironmentGQL
}

type AllProjectEnvironmentsRequest struct {
	ProjectId string
	GQL EnvironmentGQL
}

type IsEnvironmentDomainAvailableRequest struct {
	Domain string
}

type AllEnvsForEnvironmentRequest struct {
	ProjectId string
	EnvironmentId string
}

type AllEnvsForPluginRequest struct {
	ProjectId string
	EnvironmentId string
	PluginId string
}

type AllEnvsForProjectTokenRequest struct {
}

type GetWritableGithubScopesRequest struct {
}

type GetAvailableGitHubReposRequest struct {
	GQL GitHubRepoGQL
}

type GetBranchesForGitHubRepoRequest struct {
	Owner string
	Repo string
	GQL GitHubBranchGQL
}

type IsRepoNameAvailableRequest struct {
	FullRepoName string
}

type IntegrationAuthsRequest struct {
	GQL IntegrationAuthGQL
}

type IntegrationsRequest struct {
	ProjectId string
	GQL IntegrationGQL
}

type FindInviteCodeByCodeRequest struct {
	Code string
	GQL InviteCodeGQL
}

type InviteCodeRequest struct {
	ProjectId string
	Role string
	GQL InviteCodeGQL
}

type EnvironmentLogsRequest struct {
	ProjectId string
	EnvironmentId string
	GQL LogGQL
}

type ProjectMembersRequest struct {
	ProjectId string
	GQL MemberGQL
}

type MetricsForEnvironmentRequest struct {
	ProjectId string
	EnvironmentId string
	StartDate *string
	EndDate *string
	SamplingRate *int32
	GQL EnvironmentMetricsGQL
}

type MongoCollectionNamesRequest struct {
	ProjectId string
	EnvironmentId string
	PluginId string
}

type MongoCollectionDataRequest struct {
	ProjectId string
	EnvironmentId string
	PluginId string
	Name string
	GQL MongoCollectionGQL
}

type AvailablePluginsForProjectRequest struct {
	ProjectId string
}

type ProjectByIdRequest struct {
	ProjectId string
	GQL ProjectGQL
}

type AllProjectsRequest struct {
	Query *string
	Limit *int32
	Offset *int32
	GQL ProjectGQL
}

type IsProjectDomainAvailableRequest struct {
	Domain string
}

type ProjectTokensRequest struct {
	ProjectId string
	GQL ProjectTokenGQL
}

type RedisKeysRequest struct {
	ProjectId string
	EnvironmentId string
	PluginId string
	GQL RedisKeyGQL
}

type RedisGetKeyRequest struct {
	Key string
	ProjectId string
	EnvironmentId string
	PluginId string
}

type AllRequestedPluginsRequest struct {
	GQL RequestedPluginGQL
}

type RequestedPluginCountByNameRequest struct {
	Name string
}

type GetSQLTableNamesRequest struct {
	ProjectId string
	EnvironmentId string
	PluginId string
	DatabaseType string
}

type GetSQLTableRequest struct {
	ProjectId string
	EnvironmentId string
	PluginId string
	DatabaseType string
	Name string
	Limit *int32
	Offset *int32
	GQL SQLTableGQL
}

type StatsRequest struct {
	GQL StatsGQL
}

type GetSubscriptionsRequest struct {
	GQL SubscriptionGQL
}

type TeamByIdRequest struct {
	TeamId string
	GQL TeamGQL
}

type FindTeamByCodeRequest struct {
	Code string
	GQL TeamGQL
}

type AllTeamsRequest struct {
	Limit *int32
	Offset *int32
	GQL TeamGQL
}

type MeRequest struct {
	GQL UserGQL
}

type UserRestrictionsRequest struct {
	GQL UserRestrictionsGQL
}

type AllUsersRequest struct {
	Limit *int32
	Offset *int32
	Query *string
	EarlyAdopter *bool
	GQL UserGQL
}

type VerifyLoginSessionRequest struct {
	Code string
}

type VercelInfoRequest struct {
	GQL VercelInfoGQL
}

type PriceForProjectRequest struct {
	ProjectId string
	GQL ProjectPriceGQL
}

type PriceForUserProjectsRequest struct {
	UserId string
	GQL ProjectPriceGQL
}

type PriceForTeamProjectsRequest struct {
	TeamId string
	GQL ProjectPriceGQL
}

type GetWorkflowStatusRequest struct {
	WorkflowId string
	GQL WorkflowResultGQL
}

