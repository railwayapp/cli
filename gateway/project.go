package gateway

import (
	"context"
	"fmt"

	gql "github.com/machinebox/graphql"
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

	g.authorize(ctx, gqlReq)

	var resp struct {
		Project *entity.Project `json:"projectById"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.ProjectNotFound
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

	g.authorize(ctx, gqlReq)

	gqlReq.Var("name", req.Name)

	var resp struct {
		Project *entity.Project `json:"createProject"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.ProjectCreateFailed
	}
	return resp.Project, nil
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

	err := g.authorize(ctx, gqlReq)

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

	err := g.authorize(ctx, gqlReq)

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
	gqlReq := gql.NewRequest(`
		query {
			me {
				projects {
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

	// TODO build this into the GQL client
	err := g.authorize(ctx, gqlReq)

	if err != nil {
		return nil, err
	}

	var resp struct {
		Me struct {
			Projects []*entity.Project
		} `json:"me"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.ProblemFetchingProjects
	}
	return resp.Me.Projects, nil
}

func GetRailwayUrl() string {
	url := "https://railway.app"
	if configs.IsDevMode() {
		url = fmt.Sprintf("http://localhost:3000")
	}

	return url
}
