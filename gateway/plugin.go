package gateway

import (
	"context"
	"fmt"

	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (g *Gateway) GetPlugins(ctx context.Context, projectId string) ([]*entity.Plugin, error) {
	gqlReq := gql.NewRequest(`
		query ($projectId: ID!) {
			projectById(projectId: $projectId) {
				id,
				name,
			}
		}
	`)

	gqlReq.Var("projectId", projectId)

	g.authorize(ctx, gqlReq.Header)

	var resp struct {
		PluginList *entity.PluginList `json:"projectById"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, errors.ProjectConfigNotFound
	}
	fmt.Println("plug list", resp.PluginList)
	return resp.PluginList.Plugins, nil
}
