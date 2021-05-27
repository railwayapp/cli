package gateway

import (
	"context"
	"fmt"
	"net/http"

	gql "github.com/machinebox/graphql"
	configs "github.com/railwayapp/cli/configs"
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

	// Only error if no user and no project token
	if err != nil && g.cfg.RailwayProductionToken == "" {
		return err
	}

	header.Add("x-source", CLI_SOURCE_HEADER)

	if user != nil {
		header.Add("authorization", fmt.Sprintf("Bearer %s", user.Token))
	}

	if g.cfg.RailwayProductionToken != "" {
		header.Add("project-access-token", g.cfg.RailwayProductionToken)
	}

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
	gqlClient.Log = func(s string) {
		// Uncomment this for verbose query logging
		// fmt.Println(s)
	}
	return &Gateway{
		cfg:       configs.New(),
		gqlClient: gqlClient,
	}
}
