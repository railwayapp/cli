package gateway

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (g *Gateway) GetUser(ctx context.Context) (*entity.User, error) {
	gqlReq, err := g.NewRequestWithAuth(`
		query {
			me {
				id,
				email,
				name
			}
		}
	`)
	if err != nil {
		return nil, err
	}

	var resp struct {
		User *entity.User `json:"me"`
	}
	if err := gqlReq.Run(ctx, &resp); err != nil {
		return nil, err
	}
	return resp.User, nil
}

func (g *Gateway) CreateLoginSession(ctx context.Context) (string, error) {
	gqlReq := g.NewRequestWithoutAuth(`mutation { createLoginSession } `)

	var resp struct {
		Code string `json:"createLoginSession"`
	}

	if err := gqlReq.Run(ctx, &resp); err != nil {
		return "", err
	}

	return resp.Code, nil
}

func (g *Gateway) ConsumeLoginSession(ctx context.Context, code string) (string, error) {
	gqlReq := g.NewRequestWithoutAuth(`
		mutation($code: String!) { 
			consumeLoginSession(code: $code) 
		}
	`)
	gqlReq.Var("code", code)

	var resp struct {
		Token string `json:"consumeLoginSession"`
	}

	if err := gqlReq.Run(ctx, &resp); err != nil {
		return "", err
	}

	return resp.Token, nil
}

func (g *Gateway) Logout(ctx context.Context) error {
	gqlReq, err := g.NewRequestWithAuth(`mutation { logout }`)
	if err != nil {
		return err
	}

	if err := gqlReq.Run(ctx, nil); err != nil {
		return err
	}

	return nil
}
