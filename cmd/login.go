package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Login(ctx context.Context, req *entity.CommandRequest) error {
	ui.StartSpinner(&ui.SpinnerCfg{
		Message: "Logging in...",
	})
	user, err := h.ctrl.Login(ctx)
	if err != nil {
		return err
	}
	ui.StopSpinner(fmt.Sprintf("🎉 %s as %s (%s)", ui.GreenText("Logged in"), ui.MagentaText(user.Name), ui.MagentaText(user.Email)))
	return nil
}
