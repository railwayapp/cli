package cmd

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Logs(ctx context.Context, req *entity.CommandRequest) error {
	numLines, err := req.Cmd.Flags().GetInt32("num_lines")
	if err != nil {
		return err
	}
	return h.ctrl.GetActiveDeploymentLogs(ctx, numLines)
}
