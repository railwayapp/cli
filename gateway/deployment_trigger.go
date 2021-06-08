package gateway

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (g *Gateway) DeployEnvironmentTriggers(ctx context.Context, req *entity.DeployEnvironmentTriggersRequest) error {
	gqlReq, err := g.NewRequestWithAuth(ctx, `
	  	mutation($projectId: String!, $environmentId: String!) {
			deployEnvironmentTriggers(projectId: $projectId, environmentId: $environmentId)
	  	}
	`)
	if err != nil {
		return err
	}

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("environmentId", req.EnvironmentID)

	var resp struct {
		// Nothing useful here
	}

	if err := gqlReq.Run(&resp); err != nil {
		return err
	}

	return nil
}
