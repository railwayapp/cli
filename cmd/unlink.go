package cmd

import (
	"context"
	"fmt"
	"github.com/railwayapp/cli/errors"
	"os"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Unlink(ctx context.Context, _ *entity.CommandRequest) error {
	projectCfg, err := h.ctrl.GetProjectConfigs(ctx)
	if err == errors.ProjectConfigNotFound {
		fmt.Print(ui.AlertWarning("No project is currently linked"))
		os.Exit(1)
	} else if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return err
	}

	err = h.cfg.RemoveProjectConfigs(projectCfg)
	if err != nil {
		return err
	}

	fmt.Printf("🎉 Disconnected from %s\n", ui.MagentaText(project.Name))
	return nil
}
