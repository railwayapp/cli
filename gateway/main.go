package gateway

import (
	"context"
	"fmt"

	gql "github.com/machinebox/graphql"
	configs "github.com/railwayapp/cli/configs"
	"github.com/railwayapp/cli/errors"
)

type Gateway struct {
	cfg       *configs.Configs
	gqlClient *gql.Client
}

func (g *Gateway) authorize(ctx context.Context, req *gql.Request) error {
	user, err := g.cfg.GetUserConfigs()
	if err != nil {
		return err
	}
	req.Header.Add("authorization", fmt.Sprintf("Bearer %s", user.Token))
	return nil
}

func (g *Gateway) setEnvToken(ctx context.Context, req *gql.Request) error {
	if g.cfg.RailwayEnvToken == "" {
		return errors.ProductionTokenNotSet
	}

	req.Header.Add("project-access-token", g.cfg.RailwayEnvToken)
	return nil
}

func GetGQLHost() string {
	baseURL := "https://backboard.railway.app"
	if configs.IsDevMode() {
		baseURL = fmt.Sprintf("http://localhost:8082")
	}

	return fmt.Sprintf("%s/graphql", baseURL)
}

func New() *Gateway {
	gqlClient := gql.NewClient(GetGQLHost())
	return &Gateway{
		cfg:       configs.New(),
		gqlClient: gqlClient,
	}
}
