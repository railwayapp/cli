// GENERATED FILE DO NOT EDIT!!!

package gen

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	"github.com/machinebox/graphql"
)

type Client struct {
	headers map[string]string
	gql     *graphql.Client
}

func NewClient(endpoint string, opts ...graphql.ClientOption) *Client {
	gql := graphql.NewClient(endpoint, opts...)
	return &Client{
		gql:     gql,
		headers: make(map[string]string),
	}
}

func (c *Client) WithHeader(key string, value string) *Client {
	c.headers[key] = value
	return c
}

func (c *Client) doRequest(ctx context.Context, req *graphql.Request, res interface{}) error {
	for k, v := range c.headers {
		req.Header.Add(k, v)
	}
	c.headers = make(map[string]string)
	if err := c.gql.Run(ctx, req, &res); err != nil {
		return err
	}
	return nil
}

func (c *Client) asGQL(ctx context.Context, req interface{}) (*string, error) {
	mp := make(map[string]interface{})
	bytes, err := json.Marshal(req)
	if err != nil {
		return nil, err
	}
	err = json.Unmarshal(bytes, &mp)
	if err != nil {
		return nil, err
	}
	fields := []string{}
	for k, i := range mp {
		// GQL Selection
		switch i.(type) {
		case bool:
			// GQL Selection
			fields = append(fields, k)
		case map[string]interface{}:
			// Nested GQL/Struct
			nested, err := c.asGQL(ctx, i)
			if err != nil {
				return nil, err
			}
			fields = append(fields, fmt.Sprintf("%s {\n%s\n}", k, *nested))
		default:
			return nil, errors.New("Unsupported Type! Cannot generate GQL")
		}
	}
	q := strings.Join(fields, "\n")
	return &q, nil
}

func (c *Client) AllContainers(ctx context.Context, req *AllContainersRequest) ([]Container, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($limit: Int, $offset: Int) {
			allContainers(limit: $limit, offset: $offset) {
				%s
			}
		}`, *gen))
	gqlreq.Var("limit", req.Limit)
	gqlreq.Var("offset", req.Offset)
	var resp struct {
		AllContainers []Container `json:"allContainers"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("AllContainers request failed")
	}
	return resp.AllContainers, nil
}

func (c *Client) DeploymentById(ctx context.Context, req *DeploymentByIdRequest) (*Deployment, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: ID!, $deploymentId: ID!) {
			deploymentById(projectId: $projectId, deploymentId: $deploymentId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("deploymentId", req.DeploymentId)
	var resp struct {
		DeploymentById *Deployment `json:"deploymentById"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("DeploymentById request failed")
	}
	return resp.DeploymentById, nil
}

func (c *Client) AllDeploymentsForProject(ctx context.Context, req *AllDeploymentsForProjectRequest) ([]Deployment, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: ID!) {
			allDeploymentsForProject(projectId: $projectId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	var resp struct {
		AllDeploymentsForProject []Deployment `json:"allDeploymentsForProject"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("AllDeploymentsForProject request failed")
	}
	return resp.AllDeploymentsForProject, nil
}

func (c *Client) AllDeploymentsForEnvironment(ctx context.Context, req *AllDeploymentsForEnvironmentRequest) ([]Deployment, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: ID!, $environmentId: ID!) {
			allDeploymentsForEnvironment(projectId: $projectId, environmentId: $environmentId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	var resp struct {
		AllDeploymentsForEnvironment []Deployment `json:"allDeploymentsForEnvironment"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("AllDeploymentsForEnvironment request failed")
	}
	return resp.AllDeploymentsForEnvironment, nil
}

func (c *Client) AllDeployments(ctx context.Context, req *AllDeploymentsRequest) ([]Deployment, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($status: DeployStatus, $limit: Int, $offset: Int) {
			allDeployments(status: $status, limit: $limit, offset: $offset) {
				%s
			}
		}`, *gen))
	gqlreq.Var("status", req.Status)
	gqlreq.Var("limit", req.Limit)
	gqlreq.Var("offset", req.Offset)
	var resp struct {
		AllDeployments []Deployment `json:"allDeployments"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("AllDeployments request failed")
	}
	return resp.AllDeployments, nil
}

func (c *Client) GetDeploymentTriggersByProject(ctx context.Context, req *GetDeploymentTriggersByProjectRequest) ([]DeploymentTrigger, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!) {
			getDeploymentTriggersByProject(projectId: $projectId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	var resp struct {
		GetDeploymentTriggersByProject []DeploymentTrigger `json:"getDeploymentTriggersByProject"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("GetDeploymentTriggersByProject request failed")
	}
	return resp.GetDeploymentTriggersByProject, nil
}

func (c *Client) ElasticIsUp(ctx context.Context, req *ElasticIsUpRequest) (*bool, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $environmentId: String!, $pluginId: String!) {
			elasticIsUp(projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId)
		}`))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	gqlreq.Var("pluginId", req.PluginId)
	var resp struct {
		ElasticIsUp *bool `json:"elasticIsUp"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("ElasticIsUp request failed")
	}
	return resp.ElasticIsUp, nil
}

func (c *Client) ElasticGetIndicies(ctx context.Context, req *ElasticGetIndiciesRequest) ([]ElasticIndex, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $environmentId: String!, $pluginId: String!) {
			elasticGetIndicies(projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	gqlreq.Var("pluginId", req.PluginId)
	var resp struct {
		ElasticGetIndicies []ElasticIndex `json:"elasticGetIndicies"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("ElasticGetIndicies request failed")
	}
	return resp.ElasticGetIndicies, nil
}

func (c *Client) ElasticSearchIndex(ctx context.Context, req *ElasticSearchIndexRequest) (*ElasticSearchResult, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $environmentId: String!, $pluginId: String!, $search: String!, $index: String, $from: Int, $size: Int) {
			elasticSearchIndex(projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId, search: $search, index: $index, from: $from, size: $size) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	gqlreq.Var("pluginId", req.PluginId)
	gqlreq.Var("search", req.Search)
	gqlreq.Var("index", req.Index)
	gqlreq.Var("from", req.From)
	gqlreq.Var("size", req.Size)
	var resp struct {
		ElasticSearchIndex *ElasticSearchResult `json:"elasticSearchIndex"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("ElasticSearchIndex request failed")
	}
	return resp.ElasticSearchIndex, nil
}

func (c *Client) EnvironmentById(ctx context.Context, req *EnvironmentByIdRequest) (*Environment, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $environmentId: String!) {
			environmentById(projectId: $projectId, environmentId: $environmentId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	var resp struct {
		EnvironmentById *Environment `json:"environmentById"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("EnvironmentById request failed")
	}
	return resp.EnvironmentById, nil
}

func (c *Client) AllProjectEnvironments(ctx context.Context, req *AllProjectEnvironmentsRequest) ([]Environment, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!) {
			allProjectEnvironments(projectId: $projectId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	var resp struct {
		AllProjectEnvironments []Environment `json:"allProjectEnvironments"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("AllProjectEnvironments request failed")
	}
	return resp.AllProjectEnvironments, nil
}

func (c *Client) IsEnvironmentDomainAvailable(ctx context.Context, req *IsEnvironmentDomainAvailableRequest) (*bool, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($domain: String!) {
			isEnvironmentDomainAvailable(domain: $domain)
		}`))
	gqlreq.Var("domain", req.Domain)
	var resp struct {
		IsEnvironmentDomainAvailable *bool `json:"isEnvironmentDomainAvailable"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("IsEnvironmentDomainAvailable request failed")
	}
	return resp.IsEnvironmentDomainAvailable, nil
}

func (c *Client) AllEnvsForEnvironment(ctx context.Context, req *AllEnvsForEnvironmentRequest) (*map[string]interface{}, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $environmentId: String!) {
			allEnvsForEnvironment(projectId: $projectId, environmentId: $environmentId)
		}`))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	var resp struct {
		AllEnvsForEnvironment *map[string]interface{} `json:"allEnvsForEnvironment"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("AllEnvsForEnvironment request failed")
	}
	return resp.AllEnvsForEnvironment, nil
}

func (c *Client) AllEnvsForPlugin(ctx context.Context, req *AllEnvsForPluginRequest) (*map[string]interface{}, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $environmentId: String!, $pluginId: String!) {
			allEnvsForPlugin(projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId)
		}`))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	gqlreq.Var("pluginId", req.PluginId)
	var resp struct {
		AllEnvsForPlugin *map[string]interface{} `json:"allEnvsForPlugin"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("AllEnvsForPlugin request failed")
	}
	return resp.AllEnvsForPlugin, nil
}

func (c *Client) AllEnvsForProjectToken(ctx context.Context, req *AllEnvsForProjectTokenRequest) (*map[string]interface{}, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query  {
			allEnvsForProjectToken
		}`))
	var resp struct {
		AllEnvsForProjectToken *map[string]interface{} `json:"allEnvsForProjectToken"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("AllEnvsForProjectToken request failed")
	}
	return resp.AllEnvsForProjectToken, nil
}

func (c *Client) GetWritableGithubScopes(ctx context.Context, req *GetWritableGithubScopesRequest) ([]string, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query  {
			getWritableGithubScopes
		}`))
	var resp struct {
		GetWritableGithubScopes []string `json:"getWritableGithubScopes"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("GetWritableGithubScopes request failed")
	}
	return resp.GetWritableGithubScopes, nil
}

func (c *Client) GetAvailableGitHubRepos(ctx context.Context, req *GetAvailableGitHubReposRequest) ([]GitHubRepo, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query  {
			getAvailableGitHubRepos {
				%s
			}
		}`, *gen))
	var resp struct {
		GetAvailableGitHubRepos []GitHubRepo `json:"getAvailableGitHubRepos"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("GetAvailableGitHubRepos request failed")
	}
	return resp.GetAvailableGitHubRepos, nil
}

func (c *Client) GetBranchesForGitHubRepo(ctx context.Context, req *GetBranchesForGitHubRepoRequest) ([]GitHubBranch, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($owner: String!, $repo: String!) {
			getBranchesForGitHubRepo(owner: $owner, repo: $repo) {
				%s
			}
		}`, *gen))
	gqlreq.Var("owner", req.Owner)
	gqlreq.Var("repo", req.Repo)
	var resp struct {
		GetBranchesForGitHubRepo []GitHubBranch `json:"getBranchesForGitHubRepo"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("GetBranchesForGitHubRepo request failed")
	}
	return resp.GetBranchesForGitHubRepo, nil
}

func (c *Client) IsRepoNameAvailable(ctx context.Context, req *IsRepoNameAvailableRequest) (*bool, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($fullRepoName: String!) {
			isRepoNameAvailable(fullRepoName: $fullRepoName)
		}`))
	gqlreq.Var("fullRepoName", req.FullRepoName)
	var resp struct {
		IsRepoNameAvailable *bool `json:"isRepoNameAvailable"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("IsRepoNameAvailable request failed")
	}
	return resp.IsRepoNameAvailable, nil
}

func (c *Client) IntegrationAuths(ctx context.Context, req *IntegrationAuthsRequest) ([]IntegrationAuth, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query  {
			integrationAuths {
				%s
			}
		}`, *gen))
	var resp struct {
		IntegrationAuths []IntegrationAuth `json:"integrationAuths"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("IntegrationAuths request failed")
	}
	return resp.IntegrationAuths, nil
}

func (c *Client) Integrations(ctx context.Context, req *IntegrationsRequest) ([]Integration, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!) {
			integrations(projectId: $projectId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	var resp struct {
		Integrations []Integration `json:"integrations"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("Integrations request failed")
	}
	return resp.Integrations, nil
}

func (c *Client) FindInviteCodeByCode(ctx context.Context, req *FindInviteCodeByCodeRequest) (*InviteCode, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($code: String!) {
			findInviteCodeByCode(code: $code) {
				%s
			}
		}`, *gen))
	gqlreq.Var("code", req.Code)
	var resp struct {
		FindInviteCodeByCode *InviteCode `json:"findInviteCodeByCode"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("FindInviteCodeByCode request failed")
	}
	return resp.FindInviteCodeByCode, nil
}

func (c *Client) InviteCode(ctx context.Context, req *InviteCodeRequest) (*InviteCode, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $role: String!) {
			inviteCode(projectId: $projectId, role: $role) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("role", req.Role)
	var resp struct {
		InviteCode *InviteCode `json:"inviteCode"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("InviteCode request failed")
	}
	return resp.InviteCode, nil
}

func (c *Client) EnvironmentLogs(ctx context.Context, req *EnvironmentLogsRequest) ([]Log, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $environmentId: String!) {
			environmentLogs(projectId: $projectId, environmentId: $environmentId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	var resp struct {
		EnvironmentLogs []Log `json:"environmentLogs"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("EnvironmentLogs request failed")
	}
	return resp.EnvironmentLogs, nil
}

func (c *Client) ProjectMembers(ctx context.Context, req *ProjectMembersRequest) ([]Member, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: ID!) {
			projectMembers(projectId: $projectId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	var resp struct {
		ProjectMembers []Member `json:"projectMembers"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("ProjectMembers request failed")
	}
	return resp.ProjectMembers, nil
}

func (c *Client) MetricsForEnvironment(ctx context.Context, req *MetricsForEnvironmentRequest) (*EnvironmentMetrics, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $environmentId: String!, $startDate: String, $endDate: String, $samplingRate: Int) {
			metricsForEnvironment(projectId: $projectId, environmentId: $environmentId, startDate: $startDate, endDate: $endDate, samplingRate: $samplingRate) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	gqlreq.Var("startDate", req.StartDate)
	gqlreq.Var("endDate", req.EndDate)
	gqlreq.Var("samplingRate", req.SamplingRate)
	var resp struct {
		MetricsForEnvironment *EnvironmentMetrics `json:"metricsForEnvironment"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("MetricsForEnvironment request failed")
	}
	return resp.MetricsForEnvironment, nil
}

func (c *Client) MongoCollectionNames(ctx context.Context, req *MongoCollectionNamesRequest) ([]string, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $environmentId: String!, $pluginId: String!) {
			mongoCollectionNames(projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId)
		}`))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	gqlreq.Var("pluginId", req.PluginId)
	var resp struct {
		MongoCollectionNames []string `json:"mongoCollectionNames"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("MongoCollectionNames request failed")
	}
	return resp.MongoCollectionNames, nil
}

func (c *Client) MongoCollectionData(ctx context.Context, req *MongoCollectionDataRequest) (*MongoCollection, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $environmentId: String!, $pluginId: String!, $name: String!) {
			mongoCollectionData(projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId, name: $name) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	gqlreq.Var("pluginId", req.PluginId)
	gqlreq.Var("name", req.Name)
	var resp struct {
		MongoCollectionData *MongoCollection `json:"mongoCollectionData"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("MongoCollectionData request failed")
	}
	return resp.MongoCollectionData, nil
}

func (c *Client) AvailablePluginsForProject(ctx context.Context, req *AvailablePluginsForProjectRequest) ([]string, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: ID!) {
			availablePluginsForProject(projectId: $projectId)
		}`))
	gqlreq.Var("projectId", req.ProjectId)
	var resp struct {
		AvailablePluginsForProject []string `json:"availablePluginsForProject"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("AvailablePluginsForProject request failed")
	}
	return resp.AvailablePluginsForProject, nil
}

func (c *Client) ProjectById(ctx context.Context, req *ProjectByIdRequest) (*Project, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: ID!) {
			projectById(projectId: $projectId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	var resp struct {
		ProjectById *Project `json:"projectById"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("ProjectById request failed")
	}
	return resp.ProjectById, nil
}

func (c *Client) AllProjects(ctx context.Context, req *AllProjectsRequest) ([]Project, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($query: String, $limit: Int, $offset: Int) {
			allProjects(query: $query, limit: $limit, offset: $offset) {
				%s
			}
		}`, *gen))
	gqlreq.Var("query", req.Query)
	gqlreq.Var("limit", req.Limit)
	gqlreq.Var("offset", req.Offset)
	var resp struct {
		AllProjects []Project `json:"allProjects"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("AllProjects request failed")
	}
	return resp.AllProjects, nil
}

func (c *Client) IsProjectDomainAvailable(ctx context.Context, req *IsProjectDomainAvailableRequest) (*bool, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($domain: String!) {
			isProjectDomainAvailable(domain: $domain)
		}`))
	gqlreq.Var("domain", req.Domain)
	var resp struct {
		IsProjectDomainAvailable *bool `json:"isProjectDomainAvailable"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("IsProjectDomainAvailable request failed")
	}
	return resp.IsProjectDomainAvailable, nil
}

func (c *Client) ProjectTokens(ctx context.Context, req *ProjectTokensRequest) ([]ProjectToken, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!) {
			projectTokens(projectId: $projectId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	var resp struct {
		ProjectTokens []ProjectToken `json:"projectTokens"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("ProjectTokens request failed")
	}
	return resp.ProjectTokens, nil
}

func (c *Client) RedisKeys(ctx context.Context, req *RedisKeysRequest) ([]RedisKey, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $environmentId: String!, $pluginId: String!) {
			redisKeys(projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	gqlreq.Var("pluginId", req.PluginId)
	var resp struct {
		RedisKeys []RedisKey `json:"redisKeys"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("RedisKeys request failed")
	}
	return resp.RedisKeys, nil
}

func (c *Client) RedisGetKey(ctx context.Context, req *RedisGetKeyRequest) (*map[string]interface{}, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($key: String!, $projectId: String!, $environmentId: String!, $pluginId: String!) {
			redisGetKey(key: $key, projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId)
		}`))
	gqlreq.Var("key", req.Key)
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	gqlreq.Var("pluginId", req.PluginId)
	var resp struct {
		RedisGetKey *map[string]interface{} `json:"redisGetKey"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("RedisGetKey request failed")
	}
	return resp.RedisGetKey, nil
}

func (c *Client) AllRequestedPlugins(ctx context.Context, req *AllRequestedPluginsRequest) ([]RequestedPlugin, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query  {
			allRequestedPlugins {
				%s
			}
		}`, *gen))
	var resp struct {
		AllRequestedPlugins []RequestedPlugin `json:"allRequestedPlugins"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("AllRequestedPlugins request failed")
	}
	return resp.AllRequestedPlugins, nil
}

func (c *Client) RequestedPluginCountByName(ctx context.Context, req *RequestedPluginCountByNameRequest) (*int32, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($name: String!) {
			requestedPluginCountByName(name: $name)
		}`))
	gqlreq.Var("name", req.Name)
	var resp struct {
		RequestedPluginCountByName *int32 `json:"requestedPluginCountByName"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("RequestedPluginCountByName request failed")
	}
	return resp.RequestedPluginCountByName, nil
}

func (c *Client) GetSQLTableNames(ctx context.Context, req *GetSQLTableNamesRequest) ([]string, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $environmentId: String!, $pluginId: String!, $databaseType: String!) {
			getSQLTableNames(projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId, databaseType: $databaseType)
		}`))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	gqlreq.Var("pluginId", req.PluginId)
	gqlreq.Var("databaseType", req.DatabaseType)
	var resp struct {
		GetSQLTableNames []string `json:"getSQLTableNames"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("GetSQLTableNames request failed")
	}
	return resp.GetSQLTableNames, nil
}

func (c *Client) GetSQLTable(ctx context.Context, req *GetSQLTableRequest) (*SQLTable, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!, $environmentId: String!, $pluginId: String!, $databaseType: String!, $name: String!, $limit: Int, $offset: Int) {
			getSQLTable(projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId, databaseType: $databaseType, name: $name, limit: $limit, offset: $offset) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	gqlreq.Var("environmentId", req.EnvironmentId)
	gqlreq.Var("pluginId", req.PluginId)
	gqlreq.Var("databaseType", req.DatabaseType)
	gqlreq.Var("name", req.Name)
	gqlreq.Var("limit", req.Limit)
	gqlreq.Var("offset", req.Offset)
	var resp struct {
		GetSQLTable *SQLTable `json:"getSQLTable"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("GetSQLTable request failed")
	}
	return resp.GetSQLTable, nil
}

func (c *Client) Stats(ctx context.Context, req *StatsRequest) (*Stats, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query  {
			stats {
				%s
			}
		}`, *gen))
	var resp struct {
		Stats *Stats `json:"stats"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("Stats request failed")
	}
	return resp.Stats, nil
}

func (c *Client) GetSubscriptions(ctx context.Context, req *GetSubscriptionsRequest) ([]Subscription, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query  {
			getSubscriptions {
				%s
			}
		}`, *gen))
	var resp struct {
		GetSubscriptions []Subscription `json:"getSubscriptions"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("GetSubscriptions request failed")
	}
	return resp.GetSubscriptions, nil
}

func (c *Client) TeamById(ctx context.Context, req *TeamByIdRequest) (*Team, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($teamId: ID!) {
			teamById(teamId: $teamId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("teamId", req.TeamId)
	var resp struct {
		TeamById *Team `json:"teamById"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("TeamById request failed")
	}
	return resp.TeamById, nil
}

func (c *Client) FindTeamByCode(ctx context.Context, req *FindTeamByCodeRequest) (*Team, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($code: String!) {
			findTeamByCode(code: $code) {
				%s
			}
		}`, *gen))
	gqlreq.Var("code", req.Code)
	var resp struct {
		FindTeamByCode *Team `json:"findTeamByCode"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("FindTeamByCode request failed")
	}
	return resp.FindTeamByCode, nil
}

func (c *Client) AllTeams(ctx context.Context, req *AllTeamsRequest) ([]Team, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($limit: Int, $offset: Int) {
			allTeams(limit: $limit, offset: $offset) {
				%s
			}
		}`, *gen))
	gqlreq.Var("limit", req.Limit)
	gqlreq.Var("offset", req.Offset)
	var resp struct {
		AllTeams []Team `json:"allTeams"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("AllTeams request failed")
	}
	return resp.AllTeams, nil
}

func (c *Client) Me(ctx context.Context, req *MeRequest) (*User, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query  {
			me {
				%s
			}
		}`, *gen))
	var resp struct {
		Me *User `json:"me"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("Me request failed")
	}
	return resp.Me, nil
}

func (c *Client) UserRestrictions(ctx context.Context, req *UserRestrictionsRequest) (*UserRestrictions, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query  {
			userRestrictions {
				%s
			}
		}`, *gen))
	var resp struct {
		UserRestrictions *UserRestrictions `json:"userRestrictions"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("UserRestrictions request failed")
	}
	return resp.UserRestrictions, nil
}

func (c *Client) AllUsers(ctx context.Context, req *AllUsersRequest) ([]User, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($limit: Int, $offset: Int, $query: String, $earlyAdopter: Boolean) {
			allUsers(limit: $limit, offset: $offset, query: $query, earlyAdopter: $earlyAdopter) {
				%s
			}
		}`, *gen))
	gqlreq.Var("limit", req.Limit)
	gqlreq.Var("offset", req.Offset)
	gqlreq.Var("query", req.Query)
	gqlreq.Var("earlyAdopter", req.EarlyAdopter)
	var resp struct {
		AllUsers []User `json:"allUsers"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("AllUsers request failed")
	}
	return resp.AllUsers, nil
}

func (c *Client) VerifyLoginSession(ctx context.Context, req *VerifyLoginSessionRequest) (*bool, error) {
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($code: String!) {
			verifyLoginSession(code: $code)
		}`))
	gqlreq.Var("code", req.Code)
	var resp struct {
		VerifyLoginSession *bool `json:"verifyLoginSession"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("VerifyLoginSession request failed")
	}
	return resp.VerifyLoginSession, nil
}

func (c *Client) VercelInfo(ctx context.Context, req *VercelInfoRequest) (*VercelInfo, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query  {
			vercelInfo {
				%s
			}
		}`, *gen))
	var resp struct {
		VercelInfo *VercelInfo `json:"vercelInfo"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("VercelInfo request failed")
	}
	return resp.VercelInfo, nil
}

func (c *Client) PriceForProject(ctx context.Context, req *PriceForProjectRequest) (*ProjectPrice, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($projectId: String!) {
			priceForProject(projectId: $projectId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("projectId", req.ProjectId)
	var resp struct {
		PriceForProject *ProjectPrice `json:"priceForProject"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("PriceForProject request failed")
	}
	return resp.PriceForProject, nil
}

func (c *Client) PriceForUserProjects(ctx context.Context, req *PriceForUserProjectsRequest) ([]ProjectPrice, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($userId: String!) {
			priceForUserProjects(userId: $userId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("userId", req.UserId)
	var resp struct {
		PriceForUserProjects []ProjectPrice `json:"priceForUserProjects"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("PriceForUserProjects request failed")
	}
	return resp.PriceForUserProjects, nil
}

func (c *Client) PriceForTeamProjects(ctx context.Context, req *PriceForTeamProjectsRequest) ([]ProjectPrice, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($teamId: String!) {
			priceForTeamProjects(teamId: $teamId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("teamId", req.TeamId)
	var resp struct {
		PriceForTeamProjects []ProjectPrice `json:"priceForTeamProjects"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("PriceForTeamProjects request failed")
	}
	return resp.PriceForTeamProjects, nil
}

func (c *Client) GetWorkflowStatus(ctx context.Context, req *GetWorkflowStatusRequest) (*WorkflowResult, error) {
	gen, err := c.asGQL(ctx, req.GQL)
	if err != nil {
		return nil, err
	}
	gqlreq := graphql.NewRequest(fmt.Sprintf(`
		query ($workflowId: String!) {
			getWorkflowStatus(workflowId: $workflowId) {
				%s
			}
		}`, *gen))
	gqlreq.Var("workflowId", req.WorkflowId)
	var resp struct {
		GetWorkflowStatus *WorkflowResult `json:"getWorkflowStatus"`
	}
	if err := c.gql.Run(ctx, gqlreq, &resp); err != nil {
		return nil, errors.New("GetWorkflowStatus request failed")
	}
	return resp.GetWorkflowStatus, nil
}

