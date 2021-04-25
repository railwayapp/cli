package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Environment(ctx context.Context, req *entity.CommandRequest) error {
	projectID, err := h.cfg.GetProject()
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectID)
	if err != nil {
		return err
	}

	var environment *entity.Environment
	if len(req.Args) > 0 {
		// Create new environment
		environment, err = h.ctrl.CreateEnvironment(ctx, &entity.CreateEnvironmentRequest{
			Name:      req.Args[0],
			ProjectID: project.Id,
		})
		if err != nil {
			return err
		}
		fmt.Println("Environment: ", ui.BlueText(req.Args[0]))
	} else {
		// Existing environment
		environment, err = ui.PromptEnvironments(project.Environments)
		if err != nil {
			return err
		}
	}

	err = h.cfg.SetEnvironment(environment.Id)
	if err != nil {
		return err
	}

	return err
}
