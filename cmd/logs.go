package cmd

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Logs(ctx context.Context, req *entity.CommandRequest) error {
	numLines, linesErr := req.Cmd.Flags().GetInt32("lines")
	if linesErr != nil {
		return linesErr
	}
	shouldDownload, shouldDownloadErr := req.Cmd.Flags().GetBool("download")
	if shouldDownloadErr != nil {
		return shouldDownloadErr
	}
	return h.ctrl.GetActiveDeploymentLogs(ctx, numLines, shouldDownload)
}
