package gateway

import (
	"context"

	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

// GetProject returns a project of id projectId, error otherwise
func (g *Gateway) SendPanic(ctx context.Context, i interface{}) error {
	gqlReq := gql.NewRequest(`
		mutation($projectId: ID!) {
			sendCliTelemetry(projectId: $projectId) {
				id,
				meta {
					error
					projectid,
					environmentid,
					user,
				}
			}
		}
	`)
	g.authorize(ctx, gqlReq.Header)

	gqlReq.Var("name", req.Name)

	var resp struct {
		Project *entity.Project `json:"sendCliTelemetry"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.SomethingWentWrong
	}
	return resp.Project, nil
}
