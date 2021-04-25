package gateway

import (
	"context"
	"fmt"

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

	err := g.authorize(ctx, gqlReq.Header)
	if err != nil {
		return nil, err
	}
	var resp struct {
		Environment *entity.Environment `json:"createEnvironment,omitempty"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		fmt.Println(err)
		return nil, errors.CreateEnvironmentFailed
	}
	return resp.Environment, nil
}
