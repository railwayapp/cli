package gateway

import (
	"context"

	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (g *Gateway) GetPlugins(ctx context.Context, projectId string) (*entity.Project, error) {
	gqlReq := gql.NewRequest(`
		query ($projectId: ID!) {
			availablePluginsForProject(projectId: $projectId) {
				plugins {
					id,
					name,
				},
			}
		}
	`)

	gqlReq.Var("projectId", projectId)

	g.authorize(ctx, gqlReq.Header)

	var resp struct {
		Plugin *entity.Plugin `json:"availablePluginsForProject"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.ProjectConfigNotFound
	}
	return resp.Project, nil
}

func (g *Gateway) CreatePlugin(ctx context.Context, req *entity.CreatePluginRequest) (*entity.Plugin, error) {
	gqlReq := gql.NewRequest(`
		mutation($projectId: String!, $name: String!) {
			createPlugin(projectId: $projectId, name: $name) {
				id,
				name
			}
		}
	`)

	g.authorize(ctx, gqlReq.Header)

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("name", req.Plugin)

	var resp struct {
		Plugin *entity.Plugin `json:"createPlugin"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.PluginCreateFailed
	}
	return resp.Plugin, nil
}
