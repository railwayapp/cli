package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
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

	if project != nil {
		fmt.Printf("%s to project %s\n", ui.GreenText("Connected"), ui.MagentaText(project.Name))

		if projectCfg.Environment != "" {
			fmt.Printf("%s %s\n", ui.GreenText("Using environment"), ui.MagentaText(getEnvironmentNameFromID(projectCfg.Environment, project.Environments)))
		} else {
			fmt.Println("Not connected to an environment")
		}

		if len(project.Plugins) > 0 {
			fmt.Printf("%s\n", ui.GreenText("Plugins added:"))
			for i := range project.Plugins {
				fmt.Printf("%s\n", ui.MagentaText(project.Plugins[i].Name))
			}
		}
	} else if projectCfg.Project != "" {
		fmt.Println("Project not found. Maybe you need to login?")
	} else {
		fmt.Println("Not connected to a project. Run railway init.")
	}

	return nil

}
