package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
)

//bench command to test
func (h *Handler) Config(ctx context.Context, req *entity.CommandRequest) error {
	fmt.Println("configs")
	// isLoggedIn, _ := h.ctrl.IsLoggedIn(ctx)
	// fmt.Println(isLoggedIn)
	// projectCfg, err := h.cfg.GetProjectConfigs()
	// if err != nil {
	// 	return err
	// }
	// if projectCfg != nil {
	// 	fmt.Println(projectCfg)
	// } else {
	// 	fmt.Println("this is nil", projectCfg)
	// }
	err := h.cfg.SetProject("thomas the tank engine")
	if err != nil {
		return err
	}

	return nil
}
