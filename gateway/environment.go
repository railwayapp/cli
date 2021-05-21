package gateway

import (
	"context"

	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (g *Gateway) CreateEnvironment(ctx context.Context, req *entity.CreateEnvironmentRequest) (*entity.Environment, error) {
	gqlReq := gql.NewRequest(`
		mutation($name: String!, $projectId: String!) {
			createEnvironment(name: $name, projectId: $projectId) {
				id
				name
			}
		}
	`)
	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("name", req.Name)

	err := g.authorize(ctx, gqlReq)
	if err != nil {
		return nil, err
	}
	var resp struct {
		Environment *entity.Environment `json:"createEnvironment,omitempty"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.CreateEnvironmentFailed
	}
	return resp.Environment, nil
}

func (g *Gateway) CreateEphemeralEnvironment(ctx context.Context, req *entity.CreateEphemeralEnvironmentRequest) (*entity.Environment, error) {
	gqlReq := gql.NewRequest(`
		mutation($name: String!, $projectId: String!, $baseEnvironmentId: String!) {
			createEphemeralEnvironment(name: $name, projectId: $projectId, baseEnvironmentId: $baseEnvironmentId) {
				id
				name
			}
		}
	`)
	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("name", req.Name)
	gqlReq.Var("baseEnvironmentId", req.BaseEnvironmentID)

	err := g.authorize(ctx, gqlReq)
	if err != nil {
		return nil, err
	}
	var resp struct {
		Environment *entity.Environment `json:"createEphemeralEnvironment,omitempty"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.CreateEnvironmentFailed
	}
	return resp.Environment, nil
}

func (g *Gateway) DeleteEnvironment(ctx context.Context, req *entity.DeleteEnvironmentRequest) error {
	gqlReq := gql.NewRequest(`
		mutation($environmentId: String!, $projectId: String!) {
			deleteEnvironment(environmentId: $environmentId, projectId: $projectId)
		}
	`)
	gqlReq.Var("environmentId", req.EnvironmentId)
	gqlReq.Var("projectId", req.ProjectID)

	err := g.authorize(ctx, gqlReq)
	if err != nil {
		return err
	}
	var resp struct {
		Created bool `json:"createEnvironment,omitempty"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return errors.CreateEnvironmentFailed
	}
	return nil
}
