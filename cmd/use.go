package cmd

import (
	"context"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Use(ctx context.Context, req *entity.CommandRequest) error {
	projectID, err := h.cfg.GetProject()
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectID)
	if err != nil {
		return err
	}

	environment, err := ui.PromptEnvironments(project.Environments)
	if err != nil {
		return err
	}

	err = h.cfg.SetEnvironment(environment.Id)
	if err != nil {
		return err
	}

	return err
}
