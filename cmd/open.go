package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Open(ctx context.Context, req *entity.CommandRequest) error {
	if len(req.Args) == 0 {
		fmt.Println("Use railway open to open links to Railway from the CLI. Here's whats we have:")
	}

	projectCfg, err := h.cfg.GetProjectConfigs()
	if err != nil {
		return err
	}
	project, err := h.ctrl.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return err
	}
	err = h.ctrl.OpenInBrowser(ctx, req.Args, project.Id)
	if err != nil {
		return err
	}
	return nil
}
