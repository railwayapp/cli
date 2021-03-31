package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Link(ctx context.Context, req *entity.CommandRequest) error {
	if len(req.Args) > 0 {
		// projectID provided as argument
		projectID := req.Args[0]
		return h.saveProjectWithID(ctx, projectID)
	}

	isLoggedIn, err := h.ctrl.IsLoggedIn(ctx)
	if err != nil {
		return err
	}

	if isLoggedIn {
		return h.linkFromAccount(ctx, req)
	} else {
		return h.linkFromID(ctx, req)
	}
}

func (h *Handler) linkFromAccount(ctx context.Context, req *entity.CommandRequest) error {
	projects, err := h.ctrl.GetProjects(ctx)
	if err != nil {
		return err
	}

	if len(projects) == 0 {
		fmt.Printf("No Projects. Create one with %s\n", ui.GreenText("railway init"))
		return nil
	}

	project, err := ui.PromptProjects(projects)
	if err != nil {
		return err
	}

	err = h.cfg.SetNewProject(project.Id)
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

	return nil
}

func (h *Handler) linkFromID(ctx context.Context, req *entity.CommandRequest) error {
	projectID, err := ui.PromptText("Enter your project id")
	if err != nil {
		return err
	}

	return h.saveProjectWithID(ctx, projectID)
}
