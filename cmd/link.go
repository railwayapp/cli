package cmd

import (
	"context"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Link(ctx context.Context, req *entity.CommandRequest) error {
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
