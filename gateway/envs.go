package gateway

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (g *Gateway) GetEnvs(ctx context.Context, req *entity.GetEnvsRequest) (*entity.Envs, error) {
	gqlReq, err := g.NewRequestWithAuth(`
		query ($projectId: String!, $environmentId: String!) {
			allEnvsForEnvironment(projectId: $projectId, environmentId: $environmentId)
		}
	`)
	if err != nil {
		return nil, err
	}

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("environmentId", req.EnvironmentID)

	var resp struct {
		Envs *entity.Envs `json:"allEnvsForEnvironment"`
	}
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, err
	}
	return resp.Envs, nil
}

func (g *Gateway) GetEnvsForPlugin(ctx context.Context, req *entity.GetEnvsForPluginRequest) (*entity.Envs, error) {
	gqlReq, err := g.NewRequestWithAuth(`
		query ($projectId: String!, $environmentId: String!, $pluginId: String!) {
			allEnvsForPlugin(projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId)
		}
	`)
	if err != nil {
		return nil, err
	}

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("environmentId", req.EnvironmentID)
	gqlReq.Var("pluginId", req.PluginID)

	var resp struct {
		Envs *entity.Envs `json:"allEnvsForPlugin"`
	}
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, err
	}
	return resp.Envs, nil
}

func (g *Gateway) GetEnvsWithProjectToken(ctx context.Context) (*entity.Envs, error) {
	gqlReq, err := g.NewRequestWithAuth(`
	  	query {
			allEnvsForProjectToken
	  	}
	`)
	if err != nil {
		return nil, err
	}

	var resp struct {
		Envs *entity.Envs `json:"allEnvsForProjectToken"`
	}
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, err
	}

	return resp.Envs, nil
}

func (g *Gateway) UpdateEnvsForPlugin(ctx context.Context, req *entity.UpdateEnvsRequest) (*entity.Envs, error) {
	gqlReq, err := g.NewRequestWithAuth(`
	  	mutation($projectId: String!, $environmentId: String! $pluginId: String! $envs: Json!) {
			updateEnvsForPlugin(projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId, envs: $envs)
	  	}
	`)
	if err != nil {
		return nil, err
	}

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("environmentId", req.EnvironmentID)
	gqlReq.Var("pluginId", req.PluginID)
	gqlReq.Var("envs", req.Envs)

	var resp struct {
		Envs *entity.Envs `json:"updateEnvsForPlugin"`
	}
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, err
	}

	return resp.Envs, nil
}
