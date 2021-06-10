package cmd

import (
	"context"
	"fmt"

	"github.com/manifoldco/promptui"
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
		var name = req.Args[0]

		// Look for existing environment with name
		for _, projectEnvironment := range project.Environments {
			if name == projectEnvironment.Name {
				environment = projectEnvironment
			}
		}

		if (environment != nil) {
			fmt.Printf("%s Environment: %s\n", promptui.IconGood, ui.BlueText(environment.Name))
		} else {
			// Create new environment
			environment, err = h.ctrl.CreateEnvironment(ctx, &entity.CreateEnvironmentRequest{
				Name:      name,
				ProjectID: project.Id,
			})
			if err != nil {
				return err
			}
			fmt.Println("Created Environment ✅\nEnvironment: ", ui.BlueText(ui.Bold(name).String()))
		}
	} else {
		// Existing environment selector
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
