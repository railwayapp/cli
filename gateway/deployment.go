package gateway

import (
	"context"

	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (g *Gateway) GetDeploymentsForEnvironment(ctx context.Context, projectId string, environmentId string) ([]*entity.Deployment, error) {
	gqlReq := gql.NewRequest(`
		query ($projectId: ID!, $environmentId: ID!) {
			allDeploymentsForEnvironment(projectId: $projectId, environmentId: $environmentId) {
				id
				status
			}
		}
	`)

	gqlReq.Var("projectId", projectId)
	gqlReq.Var("environmentId", environmentId)

	err := g.authorize(ctx, gqlReq.Header)
	if err != nil {
		return nil, err
	}

	var resp struct {
		Deployments []*entity.Deployment `json:"allDeploymentsForEnvironment"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.DeploymentFetchingFailed
	}
	return resp.Deployments, nil
}

func (g *Gateway) GetLatestDeploymentForEnvironment(ctx context.Context, projectID string, environmentID string) (*entity.Deployment, error) {
	deployments, err := g.GetDeploymentsForEnvironment(ctx, projectID, environmentID)
	if err != nil {
		return nil, err
	}
	if len(deployments) == 0 {
		return nil, errors.NoDeploymentsFound
	}
	for _, deploy := range deployments {
		if deploy.Status != entity.STATUS_REMOVED {
			return deploy, nil
		}
	}
	return nil, errors.NoDeploymentsFound
}

func (g *Gateway) GetDeploymentByID(ctx context.Context, projectId string, deploymentId string) (*entity.Deployment, error) {
	gqlReq := gql.NewRequest(`
		query ($projectId: ID!, $deploymentId: ID!) {
			deploymentById(projectId: $projectId, deploymentId: $deploymentId) {
				id
				buildLogs
				deployLogs
				status
			}
		}
	`)
	gqlReq.Var("projectId", projectId)
	gqlReq.Var("deploymentId", deploymentId)

	err := g.authorize(ctx, gqlReq.Header)
	if err != nil {
		return nil, err
	}

	var resp struct {
		Deployment *entity.Deployment `json:"deploymentById"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.DeploymentFetchingFailed
	}
	return resp.Deployment, nil
}
