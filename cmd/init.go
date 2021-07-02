package cmd

import (
	"context"
	"errors"
	"fmt"
	"net/url"
	"strings"
	"time"

	"github.com/railwayapp/cli/entity"
	CLIErrors "github.com/railwayapp/cli/errors"
	"github.com/railwayapp/cli/ui"
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
	return h.ctrl.OpenProjectInBrowser(ctx, project.Id, environment.Id)
}

func (h *Handler) initFromTemplate(ctx context.Context, req *entity.CommandRequest) error {
	ui.StartSpinner(&ui.SpinnerCfg{
		Message: "Fetching starter templates",
	})

	starters, err := h.ctrl.GetStarters(ctx)
	ui.StopSpinner("")

	template, err := ui.PromptStarterTemplates(starters)
	if err != nil {
		return err
	}

	// Parse to get query params
	parsedUrl, err := url.ParseQuery(template.Url)
	if err != nil {
		return err
	}

	optionalEnvVars := parsedUrl.Get("optionalEnvs")
	envVars := strings.Split(parsedUrl.Get("envs"), ",")
	plugins := strings.Split(parsedUrl.Get("plugins"), ",")

	// Prepare environment variables for prompt
	starterEnvVars := make([]*entity.StarterEnvVar, 0)
	for _, variable := range envVars {
		if (variable != "") {
			var envVar = new(entity.StarterEnvVar)
			envVar.Name = variable
			envVar.Desc = parsedUrl.Get(variable + "Desc")
			envVar.Default = parsedUrl.Get(variable + "Default")
			envVar.Optional = strings.Contains(optionalEnvVars, variable)

			starterEnvVars = append(starterEnvVars, envVar)
		}
	}

	// Prepare plugins for creation
	starterPlugins := make([]string, 0)
	for _, plugin := range plugins {
		if (plugin != "") {
			starterPlugins = append(starterPlugins, plugin)
		}
	}


	// Select GitHub owner
	ui.StartSpinner(&ui.SpinnerCfg{
		Message: "Fetching GitHub scopes",
	})
	scopes, err := h.ctrl.GetWritableGithubScopes(ctx)
	if err != nil {
		return err
	}
	if len(scopes) == 0 {
		return CLIErrors.NoGitHubScopesFound
	}
	ui.StopSpinner("")

	owner, err := ui.PromptGitHubScopes(scopes)
	if err != nil {
		return err
	}

	// Enter project name
	name, err := ui.PromptProjectName()
	if err != nil {
		return err
	}

	isPrivate, err := ui.PromptIsRepoPrivate()
	if err != nil {
		return err
	}

	// Prompt for env vars (if required)
	variables, err := ui.PromptEnvVars(starterEnvVars)
	if err != nil {
		return err
	}

	// Create Railway project
	ui.StartSpinner(&ui.SpinnerCfg{
		Message: "Creating project",
	})
	creationResult, err := h.ctrl.CreateProjectFromTemplate(ctx, &entity.CreateProjectFromTemplateRequest{
		Name:      name,
		Owner:     owner,
		Template:  template.Source,
		IsPrivate: isPrivate,
		Plugins:   starterPlugins,
		Variables: variables,
	})
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, creationResult.ProjectID)
	if err != nil {
		return err
	}

	ui.StopSpinner("")

	// Wait for workflow to complete
	ui.StartSpinner(&ui.SpinnerCfg{
		Message: "Deploying project",
	})

	for {
		time.Sleep(2 * time.Second)
		workflowStatus, err := h.ctrl.GetWorkflowStatus(ctx, creationResult.WorkflowID)
		if err != nil {
			return err
		}
		if workflowStatus.IsError() {
			ui.StopSpinner("Uhh Ohh. Workflow failed!")
			return CLIErrors.WorkflowFailed
		}
		if workflowStatus.IsComplete() {
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
	return h.ctrl.OpenProjectDeploymentsInBrowser(ctx, project.Id)
}

func (h *Handler) setProject(ctx context.Context, project *entity.Project) error {
	err := h.cfg.SetNewProject(project.Id)
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

func (h *Handler) Init(ctx context.Context, req *entity.CommandRequest) error {
	if len(req.Args) > 0 {
		// NOTE: This is to support legacy `railway init <PROJECT_ID>` which should
		//  now be `railway link <PROJECT_ID>`
		return h.Link(ctx, req)
	}

	// Since init can be called by guests, ensure we can fetch a user first before calling. This prevents
	// us accidentally creating a temporary (guest) project if we have a token locally but our remote
	// session was deleted.
	_, err := h.ctrl.GetUser(ctx)
	if err != nil {
		return fmt.Errorf("%s\nRun %s", ui.RedText("Account required to init project"), ui.Bold("railway login"))
	}

	selection, err := ui.PromptInit()
	if err != nil {
		return err
	}

	switch selection {
	case ui.InitNew:
		return h.initNew(ctx, req)
	case ui.InitFromTemplate:
		return h.initFromTemplate(ctx, req)
	default:
		return errors.New("Invalid selection")
	}
}
