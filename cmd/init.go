package cmd

import (
	"context"
	"errors"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) initNew(ctx context.Context, req *entity.CommandRequest) error {
	name, err := ui.PromptText("Enter project name")
	if err != nil {
		return err
	}

	project, err := h.ctrl.CreateProject(ctx, &entity.CreateProjectRequest{
		Name: &name,
	})
	if err != nil {
		return err
	}

	err = h.cfg.SetProject(project.Id)
	if err != nil {
		return err
	}

	if len(project.Environments) > 0 {
		err = h.cfg.SetEnvironment(project.Environments[0].Id)
		if err != nil {
			return err
		}
	}

	fmt.Printf("🎉 Created project %s\n", name)
	h.ctrl.OpenProjectInBrowser(ctx, project.Id)

	return nil
}

func (h *Handler) initFromAccount(ctx context.Context, req *entity.CommandRequest) error {
	projects, err := h.ctrl.GetProjects(ctx)
	if err != nil {
		return err
	}

	project, err := ui.PromptProjects(projects)
	if err != nil {
		return err
	}

	// Todo, prompt for environment
	err = h.cfg.SetProjectConfigs(&entity.ProjectConfig{
		Project:     project.Id,
		Environment: project.Environments[0].Id,
	})

	if err != nil {
		return nil
	}

	return nil
}

func (h *Handler) initFromID(ctx context.Context, req *entity.CommandRequest) error {
	projectId, err := ui.PromptText("Enter your project id")
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectId)
	if err != nil {
		return err
	}

	err = h.cfg.SetProject(project.Id)
	if err != nil {
		return err
	}

	err = h.cfg.SetProject(projectId)
	if err != nil {
		return err
	}

	if len(project.Environments) > 0 {
		err = h.cfg.SetEnvironment(project.Environments[0].Id)
		if err != nil {
			return err
		}
	}

	fmt.Printf("Connected to project %s 🎉\n", project.Name)

	return nil
}

func (h *Handler) Init(ctx context.Context, req *entity.CommandRequest) error {
	isLoggedIn, _ := h.ctrl.IsLoggedIn(ctx)

	selection, err := ui.PromptInit(isLoggedIn)
	if err != nil {
		return err
	}

	switch selection {
	case ui.InitNew:
		return h.initNew(ctx, req)
	case ui.InitFromAccount:
		return h.initFromAccount(ctx, req)
	case ui.InitFromID:
		return h.initFromID(ctx, req)
	default:
		return errors.New("Invalid selection")
	}
}
