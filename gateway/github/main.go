package github

import (
	gql "github.com/machinebox/graphql"
)

const GH_API_URL = "https://api.github.com/graphql"

type Gateway struct {
	gqlClient *gql.Client
}

func New() *Gateway {
	gqlClient := gql.NewClient(GH_API_URL)
	return &Gateway{
		gqlClient: gqlClient,
	}
}
