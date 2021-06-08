package gateway

import (
	"context"

	"github.com/railwayapp/cli/errors"
)

// GetWritableGithubScopes returns scopes associated with Railway user
func (g *Gateway) GetWritableGithubScopes(ctx context.Context) ([]string, error) {
	gqlReq, err := g.NewRequestWithAuth(`
		query {
			getWritableGithubScopes 
		}
	`)
	if err != nil {
		return nil, err
	}

	var resp struct {
		Scopes []string `json:"getWritableGithubScopes"`
	}
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, errors.ProblemFetchingWritableGithubScopes
	}
	return resp.Scopes, nil
}
