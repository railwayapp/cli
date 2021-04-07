package gateway

import (
	"context"

	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (g *Gateway) GetDeploymentsForEnvironment(ctx context.Context, projectId string, environmentId string) ([]entity.Deployment, error) {
	gqlReq := gql.NewRequest(`
		query ($projectId: ID!, $environmentId: ID!) {
			allDeploymentsForEnvironment(projectId: $projectId, environmentId: $environmentId)
		}
	`)

	gqlReq.Var("projectId", projectId)
	gqlReq.Var("environmentId", environmentId)

	err := g.authorize(ctx, gqlReq.Header)
	if err != nil {
		return nil, err
	}

	var resp struct {
		Deployments []entity.Deployment `json:"allDeploymentsForEnvironment"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.PluginGetFailed
	}
	return resp.Deployments, nil
}
