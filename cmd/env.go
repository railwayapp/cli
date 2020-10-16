package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	ui "github.com/railwayapp/cli/ui"
)

func (h *Handler) Env(ctx context.Context, req *entity.CommandRequest) error {
	envs, err := h.ctrl.GetEnvs(ctx)
	if err != nil {
		return err
	}

	for k, v := range *envs {
		fmt.Print(ui.Colorize(fmt.Sprintf("%-15s%-15s\n", k, v)))
	}
	return nil
}
