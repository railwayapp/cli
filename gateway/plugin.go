package gateway

import (
	"context"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (g *Gateway) GetAvailablePlugins(ctx context.Context, projectId string) ([]string, error) {
	gqlReq, err := g.NewRequestWithAuth(`
		query ($projectId: ID!) {
			availablePluginsForProject(projectId: $projectId)
		}
	`)
	if err != nil {
		return nil, err
	}

	gqlReq.Var("projectId", projectId)

	var resp struct {
		Plugins []string `json:"availablePluginsForProject"`
	}
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, errors.PluginGetFailed
	}
	return resp.Plugins, nil
}

func (g *Gateway) CreatePlugin(ctx context.Context, req *entity.CreatePluginRequest) (*entity.Plugin, error) {
	gqlReq, err := g.NewRequestWithAuth(`
		mutation($projectId: String!, $name: String!) {
			createPlugin(projectId: $projectId, name: $name) {
				id,
				name
			}
		}
	`)
	if err != nil {
		return nil, err
	}

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("name", req.Plugin)

	var resp struct {
		Plugin *entity.Plugin `json:"createPlugin"`
	}
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, errors.PluginCreateFailed
	}
	return resp.Plugin, nil
}
