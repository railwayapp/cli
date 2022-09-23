package gateway

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
)

func (g *Gateway) GetEnvs(ctx context.Context, req *entity.GetEnvsRequest) (*entity.Envs, error) {
	gqlReq, err := g.NewRequestWithAuth(`
		query ($projectId: String!, $environmentId: String!, $serviceId: String!) {
			decryptedVariablesForService(projectId: $projectId, environmentId: $environmentId, serviceId: $serviceId)
		}
	`)
	if err != nil {
		return nil, err
	}

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("environmentId", req.EnvironmentID)

	if req.ServiceID != "" {
		gqlReq.Var("serviceId", req.ServiceID)
	}

	var resp struct {
		Envs *entity.Envs `json:"decryptedVariablesForService"`
	}
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, err
	}
	return resp.Envs, nil
}

func (g *Gateway) UpdateVariablesFromObject(ctx context.Context, req *entity.UpdateEnvsRequest) error {
	queryName := "upsertVariablesFromObject"

	if req.Replace {
		// When replacing, use the set query which will blow away all old variables and only set the ones in this query
		queryName = "variablesSetFromObject"
	}

	gqlReq, err := g.NewRequestWithAuth(fmt.Sprintf(`
	  	mutation($projectId: String!, $environmentId: String!, $pluginId: String, $serviceId: String, $variables: Json!) {
				%s(projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId, serviceId: $serviceId, variables: $variables)
	  	}
	`, queryName))
	if err != nil {
		return err
	}

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("environmentId", req.EnvironmentID)
	if req.PluginID != "" {
		gqlReq.Var("pluginId", req.PluginID)
	}
	if req.ServiceID != "" {
		gqlReq.Var("serviceId", req.ServiceID)
	}

	gqlReq.Var("variables", req.Envs)

	if err := gqlReq.Run(ctx, nil); err != nil {
		return err
	}

	return nil
}

func (g *Gateway) DeleteVariable(ctx context.Context, req *entity.DeleteVariableRequest) error {
	gqlReq, err := g.NewRequestWithAuth(`
	  	mutation($projectId: String!, $environmentId: String!, $pluginId: String, $serviceId: String, $name: String!) {
				deleteVariable(projectId: $projectId, environmentId: $environmentId, pluginId: $pluginId, serviceId: $serviceId, name: $name)
	  	}
	`)
	if err != nil {
		return err
	}

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("environmentId", req.EnvironmentID)
	gqlReq.Var("name", req.Name)
	if req.PluginID != "" {
		gqlReq.Var("pluginId", req.PluginID)
	}
	if req.ServiceID != "" {
		gqlReq.Var("serviceId", req.ServiceID)
	}

	if err := gqlReq.Run(ctx, nil); err != nil {
		return err
	}

	return nil
}
