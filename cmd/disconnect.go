package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) disconnectWithId(ctx context.Context, req *entity.CommandRequest) error {
	return nil
}

func (h *Handler) Disconnect(ctx context.Context, req *entity.CommandRequest) error {
	projectCfg, _ := h.cfg.GetProjectConfigs()

	project, err := h.ctrl.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return err
	}

	// fmt.Printf("%+v\n", x)
	err = h.cfg.RemoveProjectConfigs(projectCfg)
	if err != nil {
		fmt.Println("error?")
		return err
	}

	fmt.Printf("🎉 Disconnected from %s\n", ui.MagentaText(project.Name))
	return nil
}
