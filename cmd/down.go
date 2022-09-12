package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Down(ctx context.Context, req *entity.CommandRequest) error {
	isVerbose, err := req.Cmd.Flags().GetBool("verbose")

	if err != nil {
		// Verbose mode isn't a necessary flag; just default to false.
		isVerbose = false
	}

	fmt.Print(ui.VerboseInfo(isVerbose, "Using verbose mode"))

	fmt.Print(ui.VerboseInfo(isVerbose, "Loading project configuration"))
	projectConfig, err := h.ctrl.GetProjectConfigs(ctx)
	if err != nil {
		return err
	}

	fmt.Print(ui.VerboseInfo(isVerbose, "Loading environment"))
	environmentName, err := req.Cmd.Flags().GetString("environment")
	if err != nil {
		return err
	}

	environment, err := h.getEnvironment(ctx, environmentName)
	if err != nil {
		return err
	}
	fmt.Print(ui.VerboseInfo(isVerbose, fmt.Sprintf("Using environment %s", ui.Bold(environment.Name))))

	fmt.Print(ui.VerboseInfo(isVerbose, "Loading project"))
	project, err := h.ctrl.GetProject(ctx, projectConfig.Project)
	if err != nil {
		return err
	}

	bypass, err := req.Cmd.Flags().GetBool("yes")
	if err != nil {
		bypass = false
	}
	if !bypass {
		shouldDelete, err := ui.PromptYesNo(fmt.Sprintf("Delete latest deployment for project %s?", project.Name))
		if err != nil || !shouldDelete {
			return err
		}
	}

	err = h.ctrl.Down(ctx, &entity.DownRequest{
		ProjectID:     project.Id,
		EnvironmentID: environment.Id,
	})

	if err != nil {
		return err
	}

	fmt.Print(ui.AlertInfo(fmt.Sprintf("Deleted latest deployment for project %s.", project.Name)))

	return nil
}
