package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
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
	user, err := h.ctrl.GetUser(ctx)

	projectCfg, err := h.cfg.GetProjectConfigs()
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectCfg.Project)

	if user != nil {
		// user names can be empty
		if user.Name == "" {
			fmt.Println(fmt.Sprintf("Logged in as: %s", user.Email))
		} else {
			fmt.Println(fmt.Sprintf("Logged in as: %s (%s)", user.Name, user.Email))
		}
	} else {
		fmt.Println("Not logged in. Run railway login")
	}

	if project != nil {
		fmt.Println("Connected to project", project.Name)

		if projectCfg.Environment != "" {
			fmt.Println("Using environment", getEnvironmentNameFromID(projectCfg.Environment, project.Environments))
		} else {
			fmt.Println("Not connected to an environment")
		}

		if len(project.Plugins) > 0 {
			fmt.Println("Plugins added:")
			for i := range project.Plugins {
				fmt.Println(project.Plugins[i].Name)
			}
		}
	} else if projectCfg.Project != "" {
		if user != nil {
			fmt.Println("Project not found")
		} else {
			fmt.Println("Project not found. Maybe you need to login?")
		}
	} else {
		fmt.Println("Not connected to a project. Run railway init.")
	}

	return nil

}
