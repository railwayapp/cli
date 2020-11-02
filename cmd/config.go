package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Config(ctx context.Context, req *entity.CommandRequest) error {
	fmt.Println("configs")
	// isLoggedIn, _ := h.ctrl.IsLoggedIn(ctx)
	// fmt.Println(isLoggedIn)
	projectCfg, err := h.cfg.GetProjectConfigs()
	if err != nil {
		return err
	}
	if projectCfg != nil {
		fmt.Println(projectCfg)
	} else {
		fmt.Println("this is nil", projectCfg)
	}

	return nil
}
