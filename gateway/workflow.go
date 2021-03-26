package gateway

import (
	context "context"
	gql "github.com/machinebox/graphql"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (g *Gateway) GetWorkflowStatus(ctx context.Context, workflowID string) (entity.WorkflowStatus, error) {
	gqlReq := gql.NewRequest(`
		query($workflowId: String!) {
			getWorkflowStatus(workflowId: $workflowId) {
				status
			}
		}
	`)

	err := g.authorize(ctx, gqlReq.Header)
	if err != nil {
		return "", err
	}

	gqlReq.Var("workflowId", workflowID)

	var resp struct {
		WorkflowStatus *entity.WorkflowStatusResponse `json:"getWorkflowStatus"`
	}
	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return "", errors.ProjectCreateFailed
	}
	return resp.WorkflowStatus.Status, nil
}
