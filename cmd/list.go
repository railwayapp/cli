package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) List(ctx context.Context, req *entity.CommandRequest) error {
	projectId, err := h.cfg.GetProject()
	if err != nil {
		return err
	}
	projects, err := h.ctrl.GetProjects(ctx)
	if err != nil {
		return err
	}

	for _, v := range projects {
		if projectId == v.Id {
			fmt.Println(ui.MagentaText(v.Name))
			continue
		}
		fmt.Println(ui.GrayText(v.Name))
	}

	return nil
}
