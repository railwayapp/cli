package cmd

import (
	"context"
	"errors"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) saveProjectAndEnvironment(ctx context.Context, project *entity.Project) error {
	if len(project.Environments) > 1 {
		environment, err := ui.PromptEnvironments(project.Environments)
		if err != nil {
			return err
		}

		err = h.cfg.SetEnvironment(environment.Id)
		if err != nil {
			return err
		}
	} else if len(project.Environments) == 1 {
		err := h.cfg.SetEnvironment(project.Environments[0].Id)
		if err != nil {
			return err
		}
	}

	return nil
}

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

	err = h.saveProjectAndEnvironment(ctx, project)
	if err != nil {
		return err
	}

	fmt.Printf("🎉 Created project %s\n", ui.MagentaText(name))
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

	err = h.cfg.SetProject(project.Id)
	if err != nil {
		return err
	}

	err = h.saveProjectAndEnvironment(ctx, project)
	if err != nil {
		return err
	}

	fmt.Printf("🔌 %s to project %s\n", ui.MagentaText("Connected"), ui.GreenText(project.Name))

	return nil
}

func (h *Handler) saveProjectWithID(ctx context.Context, projectID string) error {
	project, err := h.ctrl.GetProject(ctx, projectID)
	if err != nil {
		return err
	}

	err = h.cfg.SetProject(project.Id)
	if err != nil {
		return err
	}

	err = h.cfg.SetProject(projectID)
	if err != nil {
		return err
	}

	err = h.saveProjectAndEnvironment(ctx, project)
	if err != nil {
		return err
	}

	fmt.Printf("🔌 %s to project %s\n", ui.MagentaText("Connected"), ui.GreenText(project.Name))

	return nil
}

func (h *Handler) initFromID(ctx context.Context, req *entity.CommandRequest) error {
	projectID, err := ui.PromptText("Enter your project id")
	if err != nil {
		return err
	}

	return h.saveProjectWithID(ctx, projectID)
}

func (h *Handler) Init(ctx context.Context, req *entity.CommandRequest) error {
	if len(req.Args) > 0 {
		// projectID provided as argument
		projectID := req.Args[0]
		return h.saveProjectWithID(ctx, projectID)
	}

	isLoggedIn, _ := h.ctrl.IsLoggedIn(ctx)

	if !isLoggedIn {
		return errors.New("Account require to init project")
	}

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
