package errors

import (
	"fmt"

	"github.com/railwayapp/cli/ui"
)

type RailwayError error

// TEST

var (
	RootConfigNotFound                  RailwayError = fmt.Errorf("Run %s to get started", ui.Bold("railway login"))
	UserConfigNotFound                  RailwayError = fmt.Errorf("%s\nRun %s", ui.RedText("Not logged in."), ui.Bold("railway login"))
	ProjectConfigNotFound               RailwayError = fmt.Errorf("%s\nRun %s to create a new project, or %s to use an existing project", ui.RedText("Project not found"), ui.Bold("railway init"), ui.Bold("railway link"))
	UserNotAuthorized                   RailwayError = fmt.Errorf("%s\nTry running %s", ui.RedText("Not authorized!"), ui.Bold("railway login"))
	ProjectTokenNotFound                RailwayError = fmt.Errorf("%s\n", ui.RedText("Project token not found"))
	ProblemFetchingProjects             RailwayError = fmt.Errorf("%s\nOne of our trains probably derailed!", ui.RedText("There was a problem fetching your projects."))
	ProblemFetchingWritableGithubScopes RailwayError = fmt.Errorf("%s\nOne of our trains probably derailed!", ui.RedText("There was a problem fetching GitHub metadata."))
	ProjectCreateFailed                 RailwayError = fmt.Errorf("%s\nOne of our trains probably derailed!", ui.RedText("There was a problem creating the project."))
	ProjectCreateFromTemplateFailed     RailwayError = fmt.Errorf("%s\nOne of our trains probably derailed!", ui.RedText("There was a problem creating the project from template."))
	ProductionTokenNotSet               RailwayError = fmt.Errorf("%s\nRun %s and head under `tokens` section. You can generate tokens to access Railway environment variables. Set that token in your environment as `RAILWAY_TOKEN=<insert token>` and you're all aboard!", ui.RedText("RAILWAY_TOKEN environment variable not set."), ui.Bold("railway open"))
	EnvironmentNotSet                   RailwayError = fmt.Errorf("%s", ui.RedText("No active environment found. Please select one"))
	EnvironmentNotFound                 RailwayError = fmt.Errorf("%s", ui.RedText("Environment does not exist on project. Specify an existing environment"))
	NoGitHubScopesFound                 RailwayError = fmt.Errorf("%s", ui.RedText("No GitHub organizations found. Please link your GitHub account to Railway and try again."))
	CommandNotSpecified                 RailwayError = fmt.Errorf("%s\nRun %s", ui.RedText("Specify a command to run inside the railway environment. Not providing a command will build and run the Dockerfile in the current directory."), ui.Bold("railway run [cmd]"))
	LoginFailed                         RailwayError = fmt.Errorf("%s", ui.RedText("Login failed"))
	LoginTimeout                        RailwayError = fmt.Errorf("%s", ui.RedText("Login timeout"))
	PluginAlreadyExists                 RailwayError = fmt.Errorf("%s", ui.RedText("Plugin already exists"))
	PluginNotSpecified                  RailwayError = fmt.Errorf("%s\nRun %s", ui.RedText("Specify a plugin to create."), ui.Bold("railway add <plugin>"))
	PluginCreateFailed                  RailwayError = fmt.Errorf("%s\nUhh Ohh! One of our trains derailed.", ui.RedText("There was a problem creating the plugin."))
	PluginGetFailed                     RailwayError = fmt.Errorf("%s\nUhh Ohh! One of our trains derailed.", ui.RedText("There was a problem getting plugins available for creation."))
	TelemetryFailed                     RailwayError = fmt.Errorf("%s", ui.RedText("One of our trains derailed. Any chance you can report this error on our Discord (https://railway.app/help)?"))
	WorkflowFailed                      RailwayError = fmt.Errorf("%s", ui.RedText("There was a problem deploying the project. Any chance you can report this error on our Discord (https://railway.app/help)?"))
	NoDeploymentsFound                  RailwayError = fmt.Errorf("%s", ui.RedText("No Deployments Found!"))
	DeploymentFetchingFailed            RailwayError = fmt.Errorf("%s", "Failed to fetch deployments")
	CreateEnvironmentFailed             RailwayError = fmt.Errorf("%s", ui.RedText("Creating environment failed!"))
	ServiceNotFound                     RailwayError = fmt.Errorf("%s", ui.RedText("Service not found in project"))
)
