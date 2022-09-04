package gateway

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (g *Gateway) Down(ctx context.Context, req *entity.DownRequest) error {
	deployment, err := g.GetLatestDeploymentForEnvironment(ctx, req.ProjectID, req.EnvironmentID)

	if err != nil {
		return err
	}

	gqlReq, err := g.NewRequestWithAuth(`
		mutation removeDeployment($projectId: ID!, $deploymentId: ID!) {
			removeDeployment(projectId: $projectId, deploymentId: $deploymentId)
		}
	`)

	if err != nil {
		return err
	}

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("deploymentId", deployment.ID)

	if err = gqlReq.Run(ctx, nil); err != nil {
		return err
	}

	return nil
}
