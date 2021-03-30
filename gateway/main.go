package gateway

import (
	"context"
	"fmt"
	"net/http"

	gql "github.com/machinebox/graphql"
	configs "github.com/railwayapp/cli/configs"
	"github.com/railwayapp/cli/errors"
)

const (
	CLI_SOURCE_HEADER = "cli"
)

type Gateway struct {
	cfg       *configs.Configs
	gqlClient *gql.Client
}

func (g *Gateway) authorize(ctx context.Context, header http.Header) error {
	user, err := g.cfg.GetUserConfigs()
	if err != nil {
		return err
	}
	header.Add("authorization", fmt.Sprintf("Bearer %s", user.Token))
	header.Add("x-source", CLI_SOURCE_HEADER)
	return nil
}

func (g *Gateway) setProjectToken(ctx context.Context, req *gql.Request) error {
	if g.cfg.RailwayProductionToken == "" {
		return errors.ProductionTokenNotSet
	}

	req.Header.Add("project-access-token", g.cfg.RailwayProductionToken)
	return nil
}

func GetHost() string {
	baseURL := "https://backboard.railway.app"
	if configs.IsDevMode() {
		baseURL = "http://localhost:8082"
	}
	return baseURL
}

func GetGQLHost() string {
	baseURL := GetHost()
	return fmt.Sprintf("%s/graphql", baseURL)
}

func New() *Gateway {
	gqlClient := gql.NewClient(GetGQLHost())
	return &Gateway{
		cfg:       configs.New(),
		gqlClient: gqlClient,
	}
}
