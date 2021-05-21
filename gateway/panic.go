package gateway

import (
	"context"

	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (g *Gateway) SendPanic(ctx context.Context, req *entity.PanicRequest) (bool, error) {
	gqlReq := gql.NewRequest(`
		mutation($command: String!, $error: String!, $stacktrace: String!, $projectId: String, $environmentId: String) {
			sendTelemetry(command: $command, error: $error, stacktrace: $stacktrace, projectId: $projectId, environmentId: $environmentId)
		}
	`)
	err := g.authorize(ctx, gqlReq)
	if err != nil {
		return false, err
	}

	gqlReq.Var("command", req.Command)
	gqlReq.Var("error", req.PanicError)
	gqlReq.Var("stacktrace", req.Stacktrace)
	gqlReq.Var("projectId", req.ProjectID)
	gqlReq.Var("environmentId", req.EnvironmentID)

	var resp struct {
		Status bool `json:"sendTelemetry"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return false, errors.TelemetryFailed
	}
	return resp.Status, nil
}
