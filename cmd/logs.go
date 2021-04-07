package cmd

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Logs(ctx context.Context, req *entity.CommandRequest) error {
	detach, err := req.Cmd.Flags().GetBool("detach")
	if err != nil {
		return err
	}
	return h.ctrl.GetActiveDeploymentLogs(ctx, detach)
}
