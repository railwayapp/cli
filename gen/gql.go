// GENERATED FILE DO NOT EDIT!!!

package gen

type MagicAuthGQL struct {
	Id     bool     `json:"id"`
	Email  bool     `json:"email"`
	UserId bool     `json:"userId"`
	User   *UserGQL `json:"user"`
}

type ProviderAuthGQL struct {
	Id       bool     `json:"id"`
	Provider bool     `json:"provider"`
	Email    bool     `json:"email"`
	Metadata bool     `json:"metadata"`
	User     *UserGQL `json:"user"`
	UserId   bool     `json:"userId"`
}

type ContainerGQL struct {
	Id            bool            `json:"id"`
	ContainerId   bool            `json:"containerId"`
	Environment   *EnvironmentGQL `json:"environment"`
	EnvironmentId bool            `json:"environmentId"`
	Envs          bool            `json:"envs"`
	Plugin        *PluginGQL      `json:"plugin"`
	PluginId      bool            `json:"pluginId"`
	CreatedAt     bool            `json:"createdAt"`
}

type CustomerGQL struct {
	Id               bool `json:"id"`
	StripeCustomerId bool `json:"stripeCustomerId"`
}

type DeployStatusGQL string

const (
	DeployStatus_BUILDING_GQL DeployStatusGQL = "BUILDING"
	DeployStatus_SUCCESS_GQL  DeployStatusGQL = "SUCCESS"
	DeployStatus_FAILED_GQL   DeployStatusGQL = "FAILED"
)

type DeploymentGQL struct {
	Id            bool            `json:"id"`
	EnvironmentId bool            `json:"environmentId"`
	Environment   *EnvironmentGQL `json:"environment"`
	CreatedAt     bool            `json:"createdAt"`
	ProjectId     bool            `json:"projectId"`
	Project       *ProjectGQL     `json:"project"`
	Status        bool            `json:"status"`
	BuildLogs     bool            `json:"buildLogs"`
	DeployLogs    bool            `json:"deployLogs"`
}

type DeploymentTriggerGQL struct {
	Id            bool `json:"id"`
	Provider      bool `json:"provider"`
	Repository    bool `json:"repository"`
	ProjectId     bool `json:"projectId"`
	Branch        bool `json:"branch"`
	EnvironmentId bool `json:"environmentId"`
}

type ElasticIndexGQL struct {
	Uuid    bool `json:"uuid"`
	Name    bool `json:"name"`
	NumDocs bool `json:"numDocs"`
}

type ElasticSearchItemGQL struct {
	Id    bool `json:"id"`
	Index bool `json:"index"`
	Type  bool `json:"type"`
	Score bool `json:"score"`
	Data  bool `json:"data"`
}

type ElasticSearchResultGQL struct {
	Time       bool                  `json:"time"`
	NumResults bool                  `json:"numResults"`
	Items      *ElasticSearchItemGQL `json:"items"`
}

type EnvironmentGQL struct {
	Id                bool             `json:"id"`
	Name              bool             `json:"name"`
	ProjectId         bool             `json:"projectId"`
	Containers        *ContainerGQL    `json:"containers"`
	Envs              *EnvsGQL         `json:"envs"`
	EnvironmentTokens *ProjectTokenGQL `json:"environmentTokens"`
	IsEphemeral       bool             `json:"isEphemeral"`
}

type EnvsGQL struct {
	Id            bool            `json:"id"`
	Envs          bool            `json:"envs"`
	EnvironmentId bool            `json:"environmentId"`
	Environment   *EnvironmentGQL `json:"environment"`
	PluginId      bool            `json:"pluginId"`
	Plugin        *PluginGQL      `json:"plugin"`
}

type GitHubRepoGQL struct {
	Id             bool `json:"id"`
	Name           bool `json:"name"`
	FullName       bool `json:"fullName"`
	InstallationId bool `json:"installationId"`
	DefaultBranch  bool `json:"defaultBranch"`
}

type GitHubBranchGQL struct {
	Name bool `json:"name"`
}

type IntegrationGQL struct {
	Id     bool `json:"id"`
	Name   bool `json:"name"`
	Config bool `json:"config"`
}

type IntegrationAuthGQL struct {
	Id         bool `json:"id"`
	Provider   bool `json:"provider"`
	ProviderId bool `json:"providerId"`
}

type InviteCodeGQL struct {
	Id        bool            `json:"id"`
	Code      bool            `json:"code"`
	CreatedAt bool            `json:"createdAt"`
	Project   *ProjectGQL     `json:"project"`
	ProjectId bool            `json:"projectId"`
	Role      *ProjectRoleGQL `json:"role"`
}

type LogGQL struct {
	Id            bool            `json:"id"`
	CreatedAt     bool            `json:"createdAt"`
	Data          bool            `json:"data"`
	Message       bool            `json:"message"`
	Environment   *EnvironmentGQL `json:"environment"`
	EnvironmentId bool            `json:"environmentId"`
}

type MemberGQL struct {
	Id    bool            `json:"id"`
	Email bool            `json:"email"`
	Role  *ProjectRoleGQL `json:"role"`
}

type MetricGQL struct {
	ContainerId      bool `json:"containerId"`
	CpuPercentVCPU   bool `json:"cpuPercentVCPU"`
	MemoryUsageBytes bool `json:"memoryUsageBytes"`
	MemoryLimitBytes bool `json:"memoryLimitBytes"`
	NetworkTxBytes   bool `json:"networkTxBytes"`
	NetworkRxBytes   bool `json:"networkRxBytes"`
	Date             bool `json:"date"`
}

type DeploymentWithMetricsGQL struct {
	Deployment *DeploymentGQL `json:"deployment"`
	Metrics    *MetricGQL     `json:"metrics"`
}

type PluginWithMetricsGQL struct {
	Plugin  *PluginGQL `json:"plugin"`
	Metrics *MetricGQL `json:"metrics"`
}

type EnvironmentMetricsGQL struct {
	Deployments *DeploymentWithMetricsGQL `json:"deployments"`
	Plugins     *PluginWithMetricsGQL     `json:"plugins"`
}

type MongoCollectionGQL struct {
	Name bool `json:"name"`
	Data bool `json:"data"`
}

type PluginTypeGQL string

const (
	PluginType_POSTGRESQL_GQL PluginTypeGQL = "postgresql"
	PluginType_MONGODB_GQL    PluginTypeGQL = "mongodb"
	PluginType_REDIS_GQL      PluginTypeGQL = "redis"
	PluginType_ELASTIC_GQL    PluginTypeGQL = "elastic"
	PluginType_MYSQL_GQL      PluginTypeGQL = "mysql"
	PluginType_MINIO_GQL      PluginTypeGQL = "minio"
	PluginType_LEVELDB_GQL    PluginTypeGQL = "leveldb"
	PluginType_LOGGER_GQL     PluginTypeGQL = "logger"
	PluginType_ENV_GQL        PluginTypeGQL = "env"
)

type PluginGQL struct {
	Id         bool           `json:"id"`
	Name       *PluginTypeGQL `json:"name"`
	Project    *ProjectGQL    `json:"project"`
	ProjectId  bool           `json:"projectId"`
	Containers *ContainerGQL  `json:"containers"`
	Envs       *EnvsGQL       `json:"envs"`
}

type ProjectGQL struct {
	Id                 bool                  `json:"id"`
	Name               bool                  `json:"name"`
	CreatedAt          bool                  `json:"createdAt"`
	UpdatedAt          bool                  `json:"updatedAt"`
	SingletonDeploys   bool                  `json:"singletonDeploys"`
	PrDeploys          bool                  `json:"prDeploys"`
	Plugins            *PluginGQL            `json:"plugins"`
	Environments       *EnvironmentGQL       `json:"environments"`
	Deployments        *DeploymentGQL        `json:"deployments"`
	ProjectPermissions *ProjectPermissionGQL `json:"projectPermissions"`
	Users              *UserGQL              `json:"users"`
	Webhooks           *ProjectWebhookGQL    `json:"webhooks"`
}

type CreateProjectFromTemplateResultGQL struct {
	ProjectId  bool `json:"projectId"`
	WorkflowId bool `json:"workflowId"`
}

type ProjectRoleGQL string

const (
	ProjectRole_ADMIN_GQL  ProjectRoleGQL = "ADMIN"
	ProjectRole_MEMBER_GQL ProjectRoleGQL = "MEMBER"
	ProjectRole_VIEWER_GQL ProjectRoleGQL = "VIEWER"
)

type ProjectPermissionGQL struct {
	Id        bool            `json:"id"`
	User      *UserGQL        `json:"user"`
	UserId    bool            `json:"userId"`
	Project   *ProjectGQL     `json:"project"`
	ProjectId bool            `json:"projectId"`
	Role      *ProjectRoleGQL `json:"role"`
}

type ProjectTokenGQL struct {
	Id            bool            `json:"id"`
	Name          bool            `json:"name"`
	DisplayToken  bool            `json:"displayToken"`
	CreatedAt     bool            `json:"createdAt"`
	Environment   *EnvironmentGQL `json:"environment"`
	EnvironmentId bool            `json:"environmentId"`
}

type ProjectWebhookGQL struct {
	Id  bool `json:"id"`
	Url bool `json:"url"`
}

type RedisKeyGQL struct {
	Name bool `json:"name"`
	Type bool `json:"type"`
}

type RequestedPluginGQL struct {
	Id        bool     `json:"id"`
	CreatedAt bool     `json:"createdAt"`
	UpdatedAt bool     `json:"updatedAt"`
	Name      bool     `json:"name"`
	Users     *UserGQL `json:"users"`
}

type SQLTableGQL struct {
	Name        bool `json:"name"`
	PrimaryKey  bool `json:"primaryKey"`
	TotalRows   bool `json:"totalRows"`
	ColumnNames bool `json:"columnNames"`
	ColumnTypes bool `json:"columnTypes"`
	Rows        bool `json:"rows"`
}

type StatsGQL struct {
	NumUsers        bool           `json:"numUsers"`
	NumProjects     bool           `json:"numProjects"`
	NumContainers   bool           `json:"numContainers"`
	NumSubscribed   bool           `json:"numSubscribed"`
	NumDeploys      bool           `json:"numDeploys"`
	NumTeams        bool           `json:"numTeams"`
	DeploysLastHour bool           `json:"deploysLastHour"`
	LatestDeploys   *DeploymentGQL `json:"latestDeploys"`
	LatestProjects  *ProjectGQL    `json:"latestProjects"`
	ActiveProjects  *ProjectGQL    `json:"activeProjects"`
	LatestUsers     *UserGQL       `json:"latestUsers"`
}

type SubscriptionGQL struct {
	Id          bool `json:"id"`
	PeriodStart bool `json:"periodStart"`
	PeriodEnd   bool `json:"periodEnd"`
	Status      bool `json:"status"`
	IsCancelled bool `json:"isCancelled"`
}

type TeamGQL struct {
	Id              bool               `json:"id"`
	Name            bool               `json:"name"`
	Projects        *ProjectGQL        `json:"projects"`
	Members         *UserGQL           `json:"members"`
	TeamPermissions *TeamPermissionGQL `json:"teamPermissions"`
	CreatedAt       bool               `json:"createdAt"`
	UpdatedAt       bool               `json:"updatedAt"`
}

type TeamRoleGQL string

const (
	TeamRole_ADMIN_GQL  TeamRoleGQL = "ADMIN"
	TeamRole_MEMBER_GQL TeamRoleGQL = "MEMBER"
)

type TeamPermissionGQL struct {
	Id        bool         `json:"id"`
	Role      *TeamRoleGQL `json:"role"`
	Team      *TeamGQL     `json:"team"`
	TeamId    bool         `json:"teamId"`
	User      *UserGQL     `json:"user"`
	UserId    bool         `json:"userId"`
	CreatedAt bool         `json:"createdAt"`
	UpdatedAt bool         `json:"updatedAt"`
}

type RegistrationStatusGQL string

const (
	RegistrationStatus_ONBOARDED_GQL  RegistrationStatusGQL = "ONBOARDED"
	RegistrationStatus_REGISTERED_GQL RegistrationStatusGQL = "REGISTERED"
	RegistrationStatus_WAITLISTED_GQL RegistrationStatusGQL = "WAITLISTED"
)

type UserGQL struct {
	Id                 bool                   `json:"id"`
	Email              bool                   `json:"email"`
	Projects           *ProjectGQL            `json:"projects"`
	ProviderAuths      *ProviderAuthGQL       `json:"providerAuths"`
	IsAdmin            bool                   `json:"isAdmin"`
	CreatedAt          bool                   `json:"createdAt"`
	RequestedPlugins   *RequestedPluginGQL    `json:"requestedPlugins"`
	RegistrationStatus *RegistrationStatusGQL `json:"registrationStatus"`
	Plan               *PlanGQL               `json:"plan"`
	Teams              *TeamGQL               `json:"teams"`
}

type UserRestrictionsGQL struct {
	MaxProjects              bool `json:"maxProjects"`
	MaxEnvironments          bool `json:"maxEnvironments"`
	MaxDeploysPerEnvironment bool `json:"maxDeploysPerEnvironment"`
	MaxPlugins               bool `json:"maxPlugins"`
}

type VercelProjectGQL struct {
	Id        bool `json:"id"`
	Name      bool `json:"name"`
	AccountId bool `json:"accountId"`
}

type VercelTeamGQL struct {
	Id       bool              `json:"id"`
	Projects *VercelProjectGQL `json:"projects"`
}

type VercelInfoGQL struct {
	UserId bool           `json:"userId"`
	Teams  *VercelTeamGQL `json:"teams"`
}

type ProjectPriceGQL struct {
	Project        *ProjectGQL `json:"project"`
	Total          bool        `json:"total"`
	Plugins        bool        `json:"plugins"`
	Deployments    bool        `json:"deployments"`
	EarliestMetric bool        `json:"earliestMetric"`
	LatestMetric   bool        `json:"latestMetric"`
}

type WorkflowStatusGQL string

const (
	WorkflowStatus_RUNNING_GQL  WorkflowStatusGQL = "Running"
	WorkflowStatus_COMPLETE_GQL WorkflowStatusGQL = "Complete"
	WorkflowStatus_ERROR_GQL    WorkflowStatusGQL = "Error"
)

type WorkflowResultGQL struct {
	Status *WorkflowStatusGQL `json:"status"`
}

type DeploymentStatusGQL string

const (
	DeploymentStatus_BUILDING_GQL  DeploymentStatusGQL = "BUILDING"
	DeploymentStatus_DEPLOYING_GQL DeploymentStatusGQL = "DEPLOYING"
	DeploymentStatus_SUCCESS_GQL   DeploymentStatusGQL = "SUCCESS"
	DeploymentStatus_FAILED_GQL    DeploymentStatusGQL = "FAILED"
	DeploymentStatus_REMOVED_GQL   DeploymentStatusGQL = "REMOVED"
)

type PlanGQL string

const (
	Plan_FREE_GQL          PlanGQL = "FREE"
	Plan_EARLY_ADOPTER_GQL PlanGQL = "EARLY_ADOPTER"
)

type SortOrderGQL string

const (
	SortOrder_ASC_GQL  SortOrderGQL = "asc"
	SortOrder_DESC_GQL SortOrderGQL = "desc"
)

type QueryModeGQL string

const (
	QueryMode_DEFAULT_GQL     QueryModeGQL = "default"
	QueryMode_INSENSITIVE_GQL QueryModeGQL = "insensitive"
)
