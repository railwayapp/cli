package gateway

import (
	"context"
	"fmt"

	"github.com/pkg/browser"
	configs "github.com/railwayapp/cli/configs"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

// GetProjectToken looks up a project and environment by the RAILWAY_TOKEN
func (g *Gateway) GetProjectToken(ctx context.Context) (*entity.ProjectToken, error) {
	if g.cfg.RailwayProductionToken == "" {
		return nil, errors.ProjectTokenNotFound
	}

	gqlReq, err := g.NewRequestWithAuth(`
		query {
			projectToken {
				projectId
				environmentId
			}
		}
	`)
	if err != nil {
		return nil, err
	}

	var resp struct {
		ProjectToken *entity.ProjectToken `json:"projectToken"`
	}
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, errors.ProjectTokenNotFound
	}
	return resp.ProjectToken, nil
}

// GetProject returns the project associated with the projectId, as well as
// it's environments, plugins, etc
func (g *Gateway) GetProject(ctx context.Context, projectId string) (*entity.Project, error) {
	gqlReq, err := g.NewRequestWithAuth(`
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
				services {
					id,
					name
				},
			}
		}
	`)
	if err != nil {
		return nil, err
	}

	gqlReq.Var("projectId", projectId)

	var resp struct {
		Project *entity.Project `json:"projectById"`
	}
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, errors.ProjectConfigNotFound
	}
	return resp.Project, nil
}

func (g *Gateway) GetProjectByName(ctx context.Context, projectName string) (*entity.Project, error) {
	gqlReq, err := g.NewRequestWithAuth(`
		query ($projectName: String!) {
			me {
				projects(where: { name: { equals: $projectName } }) {
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
		}
	`)
	if err != nil {
		return nil, err
	}

	gqlReq.Var("projectName", projectName)

	var resp struct {
		Me struct {
			Projects []*entity.Project `json:"projects"`
		} `json:"me"`
	}

	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, errors.ProjectConfigNotFound
	}

	projects := resp.Me.Projects
	if len(projects) == 0 {
		return nil, errors.ProjectConfigNotFound
	}

	return projects[0], nil
}

func (g *Gateway) CreateProject(ctx context.Context, req *entity.CreateProjectRequest) (*entity.Project, error) {
	gqlReq, err := g.NewRequestWithAuth(`
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
	if err != nil {
		return nil, err
	}

	gqlReq.Var("name", req.Name)

	var resp struct {
		Project *entity.Project `json:"createProject"`
	}
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, errors.ProjectCreateFailed
	}
	return resp.Project, nil
}

func (g *Gateway) CreateProjectFromTemplate(ctx context.Context, req *entity.CreateProjectFromTemplateRequest) (*entity.CreateProjectFromTemplateResult, error) {
	gqlReq, err := g.NewRequestWithAuth(`
		mutation($name: String!, $owner: String!, $template: String!, $isPrivate: Boolean, $plugins: [String!], $variables: Json) {
			createProjectFromTemplate(name: $name, owner: $owner, template: $template, isPrivate: $isPrivate, plugins: $plugins, variables: $variables) {
				projectId
				workflowId
			}
		}
	`)
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
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, errors.ProjectCreateFromTemplateFailed
	}
	return resp.Result, nil
}

func (g *Gateway) UpdateProject(ctx context.Context, req *entity.UpdateProjectRequest) (*entity.Project, error) {
	gqlReq, err := g.NewRequestWithAuth(`
		mutation($projectId: ID!) {
			updateProject(projectId: $projectId) {
				id,
				name
			}
		}
	`)
	if err != nil {
		return nil, err
	}

	gqlReq.Var("projectId", req.Id)
	var resp struct {
		Project *entity.Project `json:"createProject"`
	}
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, err
	}
	return resp.Project, nil
}

func (g *Gateway) DeleteProject(ctx context.Context, projectId string) error {
	gqlReq, err := g.NewRequestWithAuth(`
		mutation($projectId: String!) {
			deleteProject(projectId: $projectId)
		}
	`)
	if err != nil {
		return err
	}

	gqlReq.Var("projectId", projectId)
	var resp struct {
		Deleted bool `json:"deleteProject"`
	}
	return gqlReq.Run(ctx, &resp)
}

// GetProjects returns all projects associated with the user, as well as
// their environments associated with those projects, error otherwise
// Performs a dual join
func (g *Gateway) GetProjects(ctx context.Context) ([]*entity.Project, error) {
	projectFrag := `
		id,
		updatedAt,
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

	gqlReq, err := g.NewRequestWithAuth(fmt.Sprintf(`
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

	if err := gqlReq.Run(ctx, &resp); err != nil {
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
	return browser.OpenURL(fmt.Sprintf("%s/project/%s/%s?environmentId=%s", configs.GetRailwayURL(), projectID, path, environmentID))
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
