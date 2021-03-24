package cmd

import (
	"context"
	"errors"
	"fmt"
	"github.com/railwayapp/cli/entity"
	errors2 "github.com/railwayapp/cli/errors"
	"github.com/railwayapp/cli/ui"
	"time"
)

func (h *Handler) initNew(ctx context.Context, req *entity.CommandRequest) error {
	name, err := ui.PromptProjectName()
	if err != nil {
		return err
	}

	project, err := h.ctrl.CreateProject(ctx, &entity.CreateProjectRequest{
		Name: &name,
	})
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

	fmt.Printf("🎉 Created project %s\n", ui.MagentaText(name))
	h.ctrl.OpenProjectInBrowser(ctx, project.Id, environment.Id)

	return nil
}

func (h *Handler) initFromTemplate(ctx context.Context, req *entity.CommandRequest) error {
	// Prompt for a template to use

	template, err := ui.PromptStarterTemplates()
	if err != nil {
		return err
	}

	// Select GitHub org

	ui.StartSpinner(&ui.SpinnerCfg{Message: "Fetching GitHub scopes"})
	scopes, err := h.ctrl.GetWritableGithubScopes(ctx)
	if err != nil {
		return err
	}
	if len(scopes) == 0 {
		return errors2.NoGitHubScopesFound
	}
	ui.StopSpinner("")

	org, err := ui.PromptGitHubScopes(scopes)
	if err != nil {
		return err
	}

	// Enter project name

	name, err := ui.PromptProjectName()

	// Prompt for env vars (if required)

	variables := make(map[string]string)
	if len(template.EnvVars) > 0 {
		fmt.Printf("\n%s\n", ui.Bold("Environment Variables"))
		for _, envVar := range template.EnvVars {
			fmt.Printf("\n(%s)\n", ui.GrayText(envVar.Desc))
			v, err := ui.PromptText(envVar.Name)
			if err != nil {
				return err
			}
			variables[name] = v
		}
		fmt.Print("\n")
	}

	// Create Railway project

	ui.StartSpinner(&ui.SpinnerCfg{Message: "Creating project"})
	creationResult, err := h.ctrl.CreateProjectFromTemplate(ctx, &entity.CreateProjectFromTemplateRequest{
		Name:      name,
		Org:       org,
		Template:  template.Href,
		IsPrivate: false,
		Plugins:   template.Plugins,
		Variables: variables,
	})
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, creationResult.ProjectID)
	if err != nil {
		return err
	}

	// Wait for workflow to complete

	ui.StopSpinner("")
	ui.StartSpinner(&ui.SpinnerCfg{
		Message: "Deploying project",
	})

	for {
		time.Sleep(2 * time.Second)
		ws, err := h.ctrl.GetWorkflowStatus(ctx, creationResult.WorkflowID)
		if err != nil {
			return err
		}
		if ws.Status == "Error" {
			ui.StopSpinner("")
			return errors2.WorkflowFailed
		}
		if ws.Status == "Complete" {
			ui.StopSpinner("Project creation complete 🚀")
			break
		}
	}

	// Select environment to activate

	environment, err := ui.PromptEnvironments(project.Environments)
	if err != nil {
		return err
	}

	err = h.cfg.SetEnvironment(environment.Id)
	if err != nil {
		return err
	}

	fmt.Printf("🎉 Created project %s\n", ui.MagentaText(name))
	h.ctrl.OpenProjectInBrowser(ctx, project.Id, environment.Id)

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

func (h *Handler) saveProjectWithID(ctx context.Context, projectID string) error {
	project, err := h.ctrl.GetProject(ctx, projectID)
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
		return errors.New(fmt.Sprintf("%s\nRun %s", ui.RedText("Account require to init project"), ui.Bold("railway login")))
	}

	selection, err := ui.PromptInit(isLoggedIn)
	if err != nil {
		return err
	}

	switch selection {
	case ui.InitNew:
		return h.initNew(ctx, req)
	case ui.InitFromTemplate:
		return h.initFromTemplate(ctx, req)
	case ui.InitFromAccount:
		return h.initFromAccount(ctx, req)
	case ui.InitFromID:
		return h.initFromID(ctx, req)
	default:
		return errors.New("Invalid selection")
	}
}
