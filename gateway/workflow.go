package gateway

import (
	context "context"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (g *Gateway) GetWorkflowStatus(ctx context.Context, workflowID string) (entity.WorkflowStatus, error) {
	gqlReq, err := g.NewRequestWithAuth(ctx, `
		query($workflowId: String!) {
			getWorkflowStatus(workflowId: $workflowId) {
				status
			}
		}
	`)
	if err != nil {
		return "", err
	}

	gqlReq.Var("workflowId", workflowID)

	var resp struct {
		WorkflowStatus *entity.WorkflowStatusResponse `json:"getWorkflowStatus"`
	}
	if err := gqlReq.Run(&resp); err != nil {
		return "", errors.ProjectCreateFailed
	}
	return resp.WorkflowStatus.Status, nil
}
