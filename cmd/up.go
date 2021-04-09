package cmd

import (
	"context"
	"fmt"
	"time"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Up(ctx context.Context, req *entity.CommandRequest) error {
	ui.StartSpinner(&ui.SpinnerCfg{
		Message: "Laying tracks in the clouds...",
	})
	url, err := h.ctrl.Up(ctx)
	if err != nil {
		return err
	} else {
		ui.StopSpinner(fmt.Sprintf("☁️ Deploy available at %s\n", ui.GrayText(url)))
	}
	detach, err := req.Cmd.Flags().GetBool("detach")
	if err != nil {
		return err
	}
	if detach {
		return nil
	}

	for i := 0; i < 3; i++ {
		err = h.ctrl.GetActiveBuildLogs(ctx, 0)
		if err == nil {
			break
		}
		time.Sleep(time.Duration(i) * 250 * time.Millisecond)
	}

	fmt.Printf("\n\n======= Build Completed ======\n\n")
	return h.ctrl.GetActiveDeploymentLogs(ctx, 0)
}
