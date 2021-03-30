package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Build(ctx context.Context, req *entity.CommandRequest) error {
	if h.cfg.RailwayProductionToken == "" {
		fmt.Println("Railway env file is only generated in production")
		return nil
	}

	err := h.ctrl.SaveEnvsToFile(ctx)
	if err != nil {
		return err
	}

	fmt.Printf(`Env written to %s
Do NOT commit the env.json file. This command should only be run as a production build step.\n`, h.cfg.RailwayEnvFilePath)
	return nil
}
