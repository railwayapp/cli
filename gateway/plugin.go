package gateway

import (
	"context"

	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (g *Gateway) GetAvailablePlugins(ctx context.Context, projectId string) ([]string, error) {
	gqlReq := gql.NewRequest(`
		query ($projectId: ID!) {
			availablePluginsForProject(projectId: $projectId)
		}
	`)

	gqlReq.Var("projectId", projectId)

	err := g.authorize(ctx, gqlReq.Header)
	if err != nil {
		return nil, err
	}

	var resp struct {
		Plugins []string `json:"availablePluginsForProject"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.PluginGetFailed
	}
	return resp.Plugins, nil
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

	err := g.authorize(ctx, gqlReq.Header)
	if err != nil {
		return nil, err
	}

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
