package gateway

import (
	"context"
)

func (g *Gateway) Down(ctx context.Context, projectId, environmentId string) error {
	deployment, err := g.GetLatestDeploymentForEnvironment(ctx, projectId, environmentId)

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

	gqlReq.Var("projectId", projectId)
	gqlReq.Var("deploymentId", deployment.ID)

	if err = gqlReq.Run(ctx, nil); err != nil {
		return err
	}

	return nil
}
