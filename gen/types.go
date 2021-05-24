// GENERATED FILE DO NOT EDIT!!!

package gen

type MagicAuth struct {
	Id string `json:"id"`
	Email string `json:"email"`
	UserId string `json:"userId"`
	User *User `json:"user"`
}

type ProviderAuth struct {
	Id string `json:"id"`
	Provider string `json:"provider"`
	Email string `json:"email"`
	Metadata map[string]interface{} `json:"metadata"`
	User *User `json:"user"`
	UserId string `json:"userId"`
}

type Container struct {
	Id string `json:"id"`
	ContainerId string `json:"containerId"`
	Environment *Environment `json:"environment"`
	EnvironmentId string `json:"environmentId"`
	Envs map[string]interface{} `json:"envs"`
	Plugin *Plugin `json:"plugin"`
	PluginId string `json:"pluginId"`
	CreatedAt string `json:"createdAt"`
}

type Customer struct {
	Id string `json:"id"`
	StripeCustomerId string `json:"stripeCustomerId"`
}

// Enum Type
type DeployStatus string
// ENUM Values
const (
	DeployStatus_BUILDING DeployStatus = "BUILDING"
	DeployStatus_SUCCESS DeployStatus = "SUCCESS"
	DeployStatus_FAILED DeployStatus = "FAILED"
)

type Deployment struct {
	Id string `json:"id"`
	EnvironmentId string `json:"environmentId"`
	Environment *Environment `json:"environment"`
	CreatedAt string `json:"createdAt"`
	ProjectId string `json:"projectId"`
	Project *Project `json:"project"`
	Status *DeploymentStatus `json:"status"`
	BuildLogs string `json:"buildLogs"`
	DeployLogs string `json:"deployLogs"`
}

type DeploymentTrigger struct {
	Id string `json:"id"`
	Provider string `json:"provider"`
	Repository string `json:"repository"`
	ProjectId string `json:"projectId"`
	Branch string `json:"branch"`
	EnvironmentId string `json:"environmentId"`
}

type ElasticIndex struct {
	Uuid string `json:"uuid"`
	Name string `json:"name"`
	NumDocs int32 `json:"numDocs"`
}

type ElasticSearchItem struct {
	Id string `json:"id"`
	Index string `json:"index"`
	Type string `json:"type"`
	Score float64 `json:"score"`
	Data map[string]interface{} `json:"data"`
}

type ElasticSearchResult struct {
	Time float64 `json:"time"`
	NumResults int32 `json:"numResults"`
	Items []*ElasticSearchItem `json:"items"`
}

type Environment struct {
	Id string `json:"id"`
	Name string `json:"name"`
	ProjectId string `json:"projectId"`
	Containers []*Container `json:"containers"`
	Envs []*Envs `json:"envs"`
	EnvironmentTokens []*ProjectToken `json:"environmentTokens"`
	IsEphemeral bool `json:"isEphemeral"`
}

type Envs struct {
	Id string `json:"id"`
	Envs map[string]interface{} `json:"envs"`
	EnvironmentId string `json:"environmentId"`
	Environment *Environment `json:"environment"`
	PluginId string `json:"pluginId"`
	Plugin *Plugin `json:"plugin"`
}

type GitHubRepo struct {
	Id int32 `json:"id"`
	Name string `json:"name"`
	FullName string `json:"fullName"`
	InstallationId string `json:"installationId"`
	DefaultBranch string `json:"defaultBranch"`
}

type GitHubBranch struct {
	Name string `json:"name"`
}

type Integration struct {
	Id string `json:"id"`
	Name string `json:"name"`
	Config map[string]interface{} `json:"config"`
}

type IntegrationAuth struct {
	Id string `json:"id"`
	Provider string `json:"provider"`
	ProviderId string `json:"providerId"`
}

type InviteCode struct {
	Id string `json:"id"`
	Code string `json:"code"`
	CreatedAt string `json:"createdAt"`
	Project *Project `json:"project"`
	ProjectId string `json:"projectId"`
	Role *ProjectRole `json:"role"`
}

type Log struct {
	Id string `json:"id"`
	CreatedAt string `json:"createdAt"`
	Data map[string]interface{} `json:"data"`
	Message string `json:"message"`
	Environment *Environment `json:"environment"`
	EnvironmentId string `json:"environmentId"`
}

type Member struct {
	Id string `json:"id"`
	Email string `json:"email"`
	Role *ProjectRole `json:"role"`
}

type Metric struct {
	ContainerId string `json:"containerId"`
	CpuPercentVCPU float64 `json:"cpuPercentVCPU"`
	MemoryUsageBytes int64 `json:"memoryUsageBytes"`
	MemoryLimitBytes int64 `json:"memoryLimitBytes"`
	NetworkTxBytes int64 `json:"networkTxBytes"`
	NetworkRxBytes int64 `json:"networkRxBytes"`
	Date string `json:"date"`
}

type DeploymentWithMetrics struct {
	Deployment *Deployment `json:"deployment"`
	Metrics []*Metric `json:"metrics"`
}

type PluginWithMetrics struct {
	Plugin *Plugin `json:"plugin"`
	Metrics []*Metric `json:"metrics"`
}

type EnvironmentMetrics struct {
	Deployments []*DeploymentWithMetrics `json:"deployments"`
	Plugins []*PluginWithMetrics `json:"plugins"`
}

type MongoCollection struct {
	Name string `json:"name"`
	Data map[string]interface{} `json:"data"`
}

// Enum Type
type PluginType string
// ENUM Values
const (
	PluginType_POSTGRESQL PluginType = "postgresql"
	PluginType_MONGODB PluginType = "mongodb"
	PluginType_REDIS PluginType = "redis"
	PluginType_ELASTIC PluginType = "elastic"
	PluginType_MYSQL PluginType = "mysql"
	PluginType_MINIO PluginType = "minio"
	PluginType_LEVELDB PluginType = "leveldb"
	PluginType_LOGGER PluginType = "logger"
	PluginType_ENV PluginType = "env"
)

type Plugin struct {
	Id string `json:"id"`
	Name *PluginType `json:"name"`
	Project *Project `json:"project"`
	ProjectId string `json:"projectId"`
	Containers []*Container `json:"containers"`
	Envs []*Envs `json:"envs"`
}

type Project struct {
	Id string `json:"id"`
	Name string `json:"name"`
	CreatedAt string `json:"createdAt"`
	UpdatedAt string `json:"updatedAt"`
	SingletonDeploys bool `json:"singletonDeploys"`
	PrDeploys bool `json:"prDeploys"`
	Plugins []*Plugin `json:"plugins"`
	Environments []*Environment `json:"environments"`
	Deployments []*Deployment `json:"deployments"`
	ProjectPermissions []*ProjectPermission `json:"projectPermissions"`
	Users []*User `json:"users"`
	Webhooks []*ProjectWebhook `json:"webhooks"`
}

type CreateProjectFromTemplateResult struct {
	ProjectId string `json:"projectId"`
	WorkflowId string `json:"workflowId"`
}

// Enum Type
type ProjectRole string
// ENUM Values
const (
	ProjectRole_ADMIN ProjectRole = "ADMIN"
	ProjectRole_MEMBER ProjectRole = "MEMBER"
	ProjectRole_VIEWER ProjectRole = "VIEWER"
)

type ProjectPermission struct {
	Id string `json:"id"`
	User *User `json:"user"`
	UserId string `json:"userId"`
	Project *Project `json:"project"`
	ProjectId string `json:"projectId"`
	Role *ProjectRole `json:"role"`
}

type ProjectToken struct {
	Id string `json:"id"`
	Name string `json:"name"`
	DisplayToken string `json:"displayToken"`
	CreatedAt string `json:"createdAt"`
	Environment *Environment `json:"environment"`
	EnvironmentId string `json:"environmentId"`
}

type ProjectWebhook struct {
	Id string `json:"id"`
	Url string `json:"url"`
}

type RedisKey struct {
	Name string `json:"name"`
	Type string `json:"type"`
}

type RequestedPlugin struct {
	Id string `json:"id"`
	CreatedAt string `json:"createdAt"`
	UpdatedAt string `json:"updatedAt"`
	Name string `json:"name"`
	Users []*User `json:"users"`
}

type SQLTable struct {
	Name string `json:"name"`
	PrimaryKey string `json:"primaryKey"`
	TotalRows int32 `json:"totalRows"`
	ColumnNames []string `json:"columnNames"`
	ColumnTypes []int32 `json:"columnTypes"`
	Rows []map[string]interface{} `json:"rows"`
}

type Stats struct {
	NumUsers int32 `json:"numUsers"`
	NumProjects int32 `json:"numProjects"`
	NumContainers int32 `json:"numContainers"`
	NumSubscribed int32 `json:"numSubscribed"`
	NumDeploys int32 `json:"numDeploys"`
	NumTeams int32 `json:"numTeams"`
	DeploysLastHour int32 `json:"deploysLastHour"`
	LatestDeploys []*Deployment `json:"latestDeploys"`
	LatestProjects []*Project `json:"latestProjects"`
	ActiveProjects []*Project `json:"activeProjects"`
	LatestUsers []*User `json:"latestUsers"`
}

type Subscription struct {
	Id string `json:"id"`
	PeriodStart int32 `json:"periodStart"`
	PeriodEnd int32 `json:"periodEnd"`
	Status string `json:"status"`
	IsCancelled bool `json:"isCancelled"`
}

type Team struct {
	Id string `json:"id"`
	Name string `json:"name"`
	Projects []*Project `json:"projects"`
	Members []*User `json:"members"`
	TeamPermissions []*TeamPermission `json:"teamPermissions"`
	CreatedAt string `json:"createdAt"`
	UpdatedAt string `json:"updatedAt"`
}

// Enum Type
type TeamRole string
// ENUM Values
const (
	TeamRole_ADMIN TeamRole = "ADMIN"
	TeamRole_MEMBER TeamRole = "MEMBER"
)

type TeamPermission struct {
	Id string `json:"id"`
	Role *TeamRole `json:"role"`
	Team *Team `json:"team"`
	TeamId string `json:"teamId"`
	User *User `json:"user"`
	UserId string `json:"userId"`
	CreatedAt string `json:"createdAt"`
	UpdatedAt string `json:"updatedAt"`
}

// Enum Type
type RegistrationStatus string
// ENUM Values
const (
	RegistrationStatus_ONBOARDED RegistrationStatus = "ONBOARDED"
	RegistrationStatus_REGISTERED RegistrationStatus = "REGISTERED"
	RegistrationStatus_WAITLISTED RegistrationStatus = "WAITLISTED"
)

type User struct {
	Id string `json:"id"`
	Email string `json:"email"`
	Projects []*Project `json:"projects"`
	ProviderAuths []*ProviderAuth `json:"providerAuths"`
	IsAdmin bool `json:"isAdmin"`
	CreatedAt string `json:"createdAt"`
	RequestedPlugins []*RequestedPlugin `json:"requestedPlugins"`
	RegistrationStatus *RegistrationStatus `json:"registrationStatus"`
	Plan *Plan `json:"plan"`
	Teams []*Team `json:"teams"`
}

type UserRestrictions struct {
	MaxProjects int32 `json:"maxProjects"`
	MaxEnvironments int32 `json:"maxEnvironments"`
	MaxDeploysPerEnvironment int32 `json:"maxDeploysPerEnvironment"`
	MaxPlugins int32 `json:"maxPlugins"`
}

type VercelProject struct {
	Id string `json:"id"`
	Name string `json:"name"`
	AccountId string `json:"accountId"`
}

type VercelTeam struct {
	Id string `json:"id"`
	Projects []*VercelProject `json:"projects"`
}

type VercelInfo struct {
	UserId string `json:"userId"`
	Teams []*VercelTeam `json:"teams"`
}

type ProjectPrice struct {
	Project *Project `json:"project"`
	Total float64 `json:"total"`
	Plugins float64 `json:"plugins"`
	Deployments float64 `json:"deployments"`
	EarliestMetric string `json:"earliestMetric"`
	LatestMetric string `json:"latestMetric"`
}

// Enum Type
type WorkflowStatus string
// ENUM Values
const (
	WorkflowStatus_RUNNING WorkflowStatus = "Running"
	WorkflowStatus_COMPLETE WorkflowStatus = "Complete"
	WorkflowStatus_ERROR WorkflowStatus = "Error"
)

type WorkflowResult struct {
	Status *WorkflowStatus `json:"status"`
}

// Enum Type
type DeploymentStatus string
// ENUM Values
const (
	DeploymentStatus_BUILDING DeploymentStatus = "BUILDING"
	DeploymentStatus_DEPLOYING DeploymentStatus = "DEPLOYING"
	DeploymentStatus_SUCCESS DeploymentStatus = "SUCCESS"
	DeploymentStatus_FAILED DeploymentStatus = "FAILED"
	DeploymentStatus_REMOVED DeploymentStatus = "REMOVED"
)

// Enum Type
type Plan string
// ENUM Values
const (
	Plan_FREE Plan = "FREE"
	Plan_EARLY_ADOPTER Plan = "EARLY_ADOPTER"
)

// Enum Type
type SortOrder string
// ENUM Values
const (
	SortOrder_ASC SortOrder = "asc"
	SortOrder_DESC SortOrder = "desc"
)

// Enum Type
type QueryMode string
// ENUM Values
const (
	QueryMode_DEFAULT QueryMode = "default"
	QueryMode_INSENSITIVE QueryMode = "insensitive"
)

