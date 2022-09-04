package gateway

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (g *Gateway) DeployEnvironmentTriggers(ctx context.Context, req *entity.DeployEnvironmentTriggersRequest) error {
	gqlReq, err := g.NewRequestWithAuth(`
	  	mutation($projectId: ID!, $environmentId: ID!, $serviceId: ID!) {
			deployEnvironmentTriggers(projectId: $projectId, environmentId: $environmentId, serviceId: $serviceId)
	  	}
	`)
	if err != nil {
		return err
	}

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("environmentId", req.EnvironmentID)
	gqlReq.Var("serviceId", req.ServiceID)

	var resp struct {
		// Nothing useful here
	}

	if err := gqlReq.Run(ctx, &resp); err != nil {
		return err
	}

	return nil
}
