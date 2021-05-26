package gateway

import (
	"context"

	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/entity"
)

func (g *Gateway) DeployEnvironmentTriggers(ctx context.Context, req *entity.DeployEnvironmentTriggersRequest) error {
	gqlReq := gql.NewRequest(`
	  	mutation($projectId: String!, $environmentId: String!) {
			deployEnvironmentTriggers(projectId: $projectId, environmentId: $environmentId)
	  	}
	`)

	err := g.authorize(ctx, gqlReq.Header)
	if err != nil {
		return err
	}

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("environmentId", req.EnvironmentID)

	var resp struct {
		// Nothing useful here
	}

	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return err
	}

	return nil
}
