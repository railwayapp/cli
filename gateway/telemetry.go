package controller

import (
	"context"

	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/errors"
)

// GetProject returns a project of id projectId, error otherwise
func (g *Gateway) SendErrors(ctx context.Context, err error) error {
	gqlReq := gql.NewRequest(`
		mutation($projectId: String) {
			insert error here
		}
	`)

	g.authorize(ctx, gqlReq.Header)

	if err := g.gqlClient.Run(ctx, gqlReq); err != nil {
		return nil, errors.TelemetryFailed
	}
	return nil
}
