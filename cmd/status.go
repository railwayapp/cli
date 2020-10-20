package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
	"github.com/railwayapp/cli/ui"
)

func getEnvironmentNameFromID(id string, environments []*entity.Environment) string {
	for _, environment := range environments {
		if environment.Id == id {
			return environment.Name
		}
	}
	return ""
}

func (h *Handler) Status(ctx context.Context, req *entity.CommandRequest) error {
	projectCfg, err := h.cfg.GetProjectConfigs()
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return err
	}

	if project != nil {
		fmt.Printf("Project: %s\n", ui.Bold(fmt.Sprint(ui.MagentaText(project.Name))))

		if projectCfg.Environment != "" {
			fmt.Printf("Environment: %s\n", ui.Bold(fmt.Sprint(ui.BlueText(getEnvironmentNameFromID(projectCfg.Environment, project.Environments)))))
		} else {
			fmt.Println(ui.RedText("Not connected to an environment"))
		}

		if len(project.Plugins) > 0 {
			fmt.Printf("Plugins:\n")
			for i := range project.Plugins {
				fmt.Printf("%s\n", ui.Bold(fmt.Sprint(ui.GrayText(project.Plugins[i].Name))))
			}
		}
	} else {
		fmt.Println(errors.ProjectConfigNotFound)
	}

	return nil

}
