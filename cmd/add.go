package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Add(ctx context.Context, req *entity.CommandRequest) error {
	projectCfg, err := h.cfg.GetProjectConfigs()
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return err
	}

	plgList, err := h.ctrl.GetPlugins(ctx, project.Id)
	fmt.Println(plgList)
	return nil
}
