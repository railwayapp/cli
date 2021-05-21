package gateway

import (
	"context"
	"fmt"

	gql "github.com/machinebox/graphql"
	"github.com/pkg/browser"
	configs "github.com/railwayapp/cli/configs"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

// GetProject returns the project associated with the projectId, as well as
// it's environments, plugins, etc
func (g *Gateway) GetProject(ctx context.Context, projectId string) (*entity.Project, error) {
	gqlReq := gql.NewRequest(`
		query ($projectId: ID!) {
			projectById(projectId: $projectId) {
				id,
				name,
				plugins {
					id,
					name,
				},
				environments {
					id,
					name
				},
			}
		}
	`)

	gqlReq.Var("projectId", projectId)

	err := g.authorize(ctx, gqlReq.Header)
	if err != nil {
		return nil, err
	}

	var resp struct {
		Project *entity.Project `json:"projectById"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.ProjectConfigNotFound
	}
	return resp.Project, nil
}

func (g *Gateway) CreateProject(ctx context.Context, req *entity.CreateProjectRequest) (*entity.Project, error) {
	gqlReq := gql.NewRequest(`
		mutation($name: String) {
			createProject(name: $name) {
				id,
				name
				environments {
					id
					name
				}
			}
		}
	`)

	err := g.authorize(ctx, gqlReq.Header)
	if err != nil {
		return nil, err
	}

	gqlReq.Var("name", req.Name)

	var resp struct {
		Project *entity.Project `json:"createProject"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.ProjectCreateFailed
	}
	return resp.Project, nil
}

func (g *Gateway) CreateProjectFromTemplate(ctx context.Context, req *entity.CreateProjectFromTemplateRequest) (*entity.CreateProjectFromTemplateResult, error) {
	gqlReq := gql.NewRequest(`
		mutation($name: String!, $owner: String!, $template: String!, $isPrivate: Boolean, $plugins: [String!], $variables: Json) {
			createProjectFromTemplate(name: $name, owner: $owner, template: $template, isPrivate: $isPrivate, plugins: $plugins, variables: $variables) {
				projectId
				workflowId
			}
		}
	`)

	err := g.authorize(ctx, gqlReq.Header)
	if err != nil {
		return nil, err
	}

	gqlReq.Var("name", req.Name)
	gqlReq.Var("owner", req.Owner)
	gqlReq.Var("template", req.Template)
	gqlReq.Var("isPrivate", req.IsPrivate)
	gqlReq.Var("plugins", req.Plugins)
	gqlReq.Var("variables", req.Variables)

	var resp struct {
		Result *entity.CreateProjectFromTemplateResult `json:"createProjectFromTemplate"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.ProjectCreateFromTemplateFailed
	}
	return resp.Result, nil
}

func (g *Gateway) UpdateProject(ctx context.Context, req *entity.UpdateProjectRequest) (*entity.Project, error) {
	gqlReq := gql.NewRequest(`
		mutation($projectId: ID!) {
			updateProject(projectId: $projectId) {
				id,
				name
			}
		}
	`)

	err := g.authorize(ctx, gqlReq.Header)

	if err != nil {
		return nil, err
	}

	gqlReq.Var("projectId", req.Id)
	var resp struct {
		Project *entity.Project `json:"createProject"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, err
	}
	return resp.Project, nil
}

func (g *Gateway) DeleteProject(ctx context.Context, projectId string) error {
	gqlReq := gql.NewRequest(`
		mutation($projectId: ID!) {
			deleteProject(projectId: $projectId)
		}
	`)

	err := g.authorize(ctx, gqlReq.Header)

	if err != nil {
		return err
	}

	gqlReq.Var("projectId", projectId)
	var resp struct {
		Deleted bool `json:"deleteProject"`
	}
	return g.gqlClient.Run(ctx, gqlReq, &resp)
}

// GetProjects returns all projects associated with the user, as well as
// their environments associated with those projects, error otherwise
// Performs a dual join
func (g *Gateway) GetProjects(ctx context.Context) ([]*entity.Project, error) {
	projectFrag := `
		id,
		name,
		plugins {
			id,
			name,
		},
		environments {
			id,
			name
		},
	`

	gqlReq := gql.NewRequest(fmt.Sprintf(`
		query {
			me {
				name
				projects {
					%s
			  }
				teams {
					name
					projects {
						%s
					}
				}
			}
		}
	`, projectFrag, projectFrag))

	// TODO build this into the GQL client
	err := g.authorize(ctx, gqlReq.Header)

	if err != nil {
		return nil, err
	}

	var resp struct {
		Me struct {
			Name     *string           `json:"name"`
			Projects []*entity.Project `json:"projects"`
			Teams    []*struct {
				Name     string            `json:"name"`
				Projects []*entity.Project `json:"projects"`
			} `json:"teams"`
		} `json:"me"`
	}

	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.ProblemFetchingProjects
	}

	projects := resp.Me.Projects

	for _, project := range resp.Me.Projects {
		name := "Me"
		if resp.Me.Name != nil {
			name = *resp.Me.Name
		}
		project.Team = &name
	}
	for _, team := range resp.Me.Teams {
		for _, project := range team.Projects {
			project.Team = &team.Name
		}
		projects = append(projects, team.Projects...)
	}

	return projects, nil
}

func (g *Gateway) OpenProjectInBrowser(projectID string, environmentID string) error {
	return browser.OpenURL(fmt.Sprintf("%s/project/%s?environmentId=%s", configs.GetRailwayURL(), projectID, environmentID))
}

func (g *Gateway) OpenProjectPathInBrowser(projectID string, environmentID string, path string) error {
	return browser.OpenURL(fmt.Sprintf("%s/project/%s/%s?environmentId=%s", GetRailwayUrl(), projectID, path, environmentID))
}

func (g *Gateway) OpenProjectDeploymentsInBrowser(projectID string) error {
	return browser.OpenURL(g.GetProjectDeploymentsURL(projectID))
}

func (g *Gateway) GetProjectDeploymentsURL(projectID string) string {
	return fmt.Sprintf("%s/project/%s/deployments?open=true", configs.GetRailwayURL(), projectID)
}

func (g *Gateway) OpenStaticUrlInBrowser(staticUrl string) error {
	return browser.OpenURL(fmt.Sprintf("https://%s", staticUrl))
}
