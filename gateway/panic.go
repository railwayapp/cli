package gateway

import (
	"context"
	"fmt"

	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (g *Gateway) SendPanic(ctx context.Context, req *entity.PanicRequest) (bool, error) {
	gqlReq := gql.NewRequest(`
		mutation($command: String, $panicErr: String, $projectId: String, $environmentId: String) {
			sendTelemetry(command: $command, panicErr: $panicErr, projectId: $projectId, environmenteId: $environemntId)
		}
	`)
	g.authorize(ctx, gqlReq.Header)

	gqlReq.Var("command", req.Command)
	gqlReq.Var("panicErr", req.PanicError)
	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("environmentId", req.EnvironmentID)

	var resp struct {
		Status bool `json:"sendTelemetry"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		//TODO: rm this line
		fmt.Println(err)
		return false, errors.TelemetryFailed
	}
	return resp.Status, nil
}
