package cmd

import (
	"context"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Use(ctx context.Context, req *entity.CommandRequest) error {
	projectId, err := h.cfg.GetProject()
	if err != nil {
		return err
	}
	project, err := h.ctrl.GetProject(ctx, projectId)
	if err != nil {
		return err
	}
	_, err = ui.PromptEnvironments(project.Environments)
	return err
}
