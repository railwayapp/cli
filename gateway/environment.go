package gateway

import (
	"context"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (g *Gateway) CreateEnvironment(ctx context.Context, req *entity.CreateEnvironmentRequest) (*entity.Environment, error) {
	gqlReq, err := g.NewRequestWithAuth(ctx, `
		mutation($name: String!, $projectId: String!) {
			createEnvironment(name: $name, projectId: $projectId) {
				id
				name
			}
		}
	`)
	if err != nil {
		return nil, err
	}

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("name", req.Name)

	var resp struct {
		Environment *entity.Environment `json:"createEnvironment,omitempty"`
	}
	if err := gqlReq.Run(&resp); err != nil {
		return nil, errors.CreateEnvironmentFailed
	}
	return resp.Environment, nil
}

func (g *Gateway) CreateEphemeralEnvironment(ctx context.Context, req *entity.CreateEphemeralEnvironmentRequest) (*entity.Environment, error) {
	gqlReq, err := g.NewRequestWithAuth(ctx, `
		mutation($name: String!, $projectId: String!, $baseEnvironmentId: String!) {
			createEphemeralEnvironment(name: $name, projectId: $projectId, baseEnvironmentId: $baseEnvironmentId) {
				id
				name
			}
		}
	`)
	if err != nil {
		return nil, err
	}

	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("name", req.Name)
	gqlReq.Var("baseEnvironmentId", req.BaseEnvironmentID)

	var resp struct {
		Environment *entity.Environment `json:"createEphemeralEnvironment,omitempty"`
	}
	if err := gqlReq.Run(&resp); err != nil {
		return nil, errors.CreateEnvironmentFailed
	}
	return resp.Environment, nil
}

func (g *Gateway) DeleteEnvironment(ctx context.Context, req *entity.DeleteEnvironmentRequest) error {
	gqlReq, err := g.NewRequestWithAuth(ctx, `
		mutation($environmentId: String!, $projectId: String!) {
			deleteEnvironment(environmentId: $environmentId, projectId: $projectId)
		}
	`)
	if err != nil {
		return err
	}

	gqlReq.Var("environmentId", req.EnvironmentId)
	gqlReq.Var("projectId", req.ProjectID)

	var resp struct {
		Created bool `json:"createEnvironment,omitempty"`
	}
	if err := gqlReq.Run(&resp); err != nil {
		return errors.CreateEnvironmentFailed
	}
	return nil
}
