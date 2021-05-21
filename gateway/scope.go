package gateway

import (
	"context"

	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/errors"
)

// GetWritableGithubScopes returns scopes associated with Railway user
func (g *Gateway) GetWritableGithubScopes(ctx context.Context) ([]string, error) {
	gqlReq := gql.NewRequest(`
		query {
			getWritableGithubScopes 
		}
	`)

	err := g.authorize(ctx, gqlReq.Header)

	if err != nil {
		return nil, err
	}

	var resp struct {
		Scopes []string `json:"getWritableGithubScopes"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.ProblemFetchingWritableGithubScopes
	}
	return resp.Scopes, nil
}
