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

	err := g.authorize(ctx, gqlReq.Header)
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

func (g *Gateway) CreateLoginSession(ctx context.Context) (string, error) {
	gqlReq := gql.NewRequest(`mutation { createLoginSession } `)

	var resp struct {
		Code string `json:"createLoginSession"`
	}

	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return "", err
	}

	return resp.Code, nil
}

func (g *Gateway) ConsumeLoginSession(ctx context.Context, code string) (string, error) {
	gqlReq := gql.NewRequest(`
  	mutation($code: String!) { 
  		consumeLoginSession(code: $code) 
  	}
	`)
	gqlReq.Var("code", code)

	var resp struct {
		Token string `json:"consumeLoginSession"`
	}

	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return "", err
	}

	return resp.Token, nil
}

func (g *Gateway) Logout(ctx context.Context) error {
	gqlReq := gql.NewRequest(`mutation { logout }`)

	err := g.authorize(ctx, gqlReq.Header)
	if err != nil {
		return err
	}

	if err := g.gqlClient.Run(ctx, gqlReq, nil); err != nil {
		return err
	}

	return nil
}
