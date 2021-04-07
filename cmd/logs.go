package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Logs(ctx context.Context, req *entity.CommandRequest) error {
	isLoggedIn, _ := h.ctrl.IsLoggedIn(ctx)

	if !isLoggedIn {
		return fmt.Errorf("%s\nRun %s", ui.RedText("Account require to init project"), ui.Bold("railway login"))
	}

	h.ctrl.GetActiveDeploymentLogs(ctx)
	return nil
}
