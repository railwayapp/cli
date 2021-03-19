package cmd

import (
	"context"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Protect(ctx context.Context, req *entity.CommandRequest) error {
	projectConfigs, err := h.cfg.GetProjectConfigs()
	if err != nil {
		return err
	}

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

	projectConfigs.LockedEnvsNames[environment.Name] = true

	err = h.cfg.SetProjectConfigs(projectConfigs)
	if err != nil {
		return err
	}

	return err
}
