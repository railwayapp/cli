package cmd

import (
	"context"
	"fmt"

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
	return nil
}
