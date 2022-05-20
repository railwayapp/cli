package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Status(ctx context.Context, req *entity.CommandRequest) error {
	projectCfg, err := h.ctrl.GetProjectConfigs(ctx)
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return err
	}

	if project != nil {
		fmt.Printf("Project: %s\n", ui.Bold(fmt.Sprint(ui.MagentaText(project.Name))))

		environment, err := h.ctrl.GetCurrentEnvironment(ctx)
		if err != nil {
			return err
		}

		fmt.Printf("Environment: %s\n", ui.Bold(fmt.Sprint(ui.BlueText(environment.Name))))

		if len(project.Plugins) > 0 {
			fmt.Printf("Plugins:\n")
			for i := range project.Plugins {
				plugin := project.Plugins[i]
				if plugin.Name == "env" {
					// legacy plugin
					continue
				}
				fmt.Printf("%s\n", ui.Bold(fmt.Sprint(ui.GrayText(plugin.Name))))
			}
		}

		if len(project.Services) > 0 {
			fmt.Printf("Services:\n")
			for i := range project.Services {
				fmt.Printf("%s\n", ui.Bold(fmt.Sprint(ui.GrayText(project.Services[i].Name))))
			}
		}
	} else {
		fmt.Println(errors.ProjectConfigNotFound)
	}

	return nil

}
