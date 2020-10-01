package gateway

import (
	"context"

	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/entity"
)

func (g *Gateway) GetUser(ctx context.Context) (*entity.User, error) {
	gqlReq := gql.NewRequest(`
	query {
		me {
			id,
			email,
			name
		}
	}
	`)

	err := g.authorize(ctx, gqlReq)

	if err != nil {
		return nil, err
	}

	var resp struct {
		User *entity.User `json:"me"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, err
	}
	return resp.User, nil
}
