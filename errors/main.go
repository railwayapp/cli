package errors

import (
	"fmt"

	"github.com/railwayapp/cli/ui"
)

type RailwayError error

// TEST

var (
	UserConfigNotFound                  RailwayError = fmt.Errorf("%s\nRun %s", ui.RedText("Not logged in."), ui.Bold("railway login"))
	ProjectConfigNotFound               RailwayError = fmt.Errorf("%s. Tip: If you haven't, do railway login\nOtherwise, run %s to get plugged into a new project, or %s to get plugged into an existing project.", ui.RedText("Project not found."), ui.Bold("railway init"), ui.Bold("railway link"))
	ProblemFetchingProjects             RailwayError = fmt.Errorf("%s\nOne of our trains probably derailed!", ui.RedText("There was a problem fetching your projects."))
	ProblemFetchingWritableGithubScopes RailwayError = fmt.Errorf("%s\nOne of our trains probably derailed!", ui.RedText("There was a problem fetching GitHub metadata."))
	ProjectCreateFailed                 RailwayError = fmt.Errorf("%s\nOne of our trains probably derailed!", ui.RedText("There was a problem creating the project."))
	ProjectCreateFromTemplateFailed     RailwayError = fmt.Errorf("%s\nOne of our trains probably derailed!", ui.RedText("There was a problem creating the project from template."))
	ProductionTokenNotSet               RailwayError = fmt.Errorf("%s\nRun %s and head under `tokens` section. You can generate tokens to access Railway environment variables. Set that token in your environment as `RAILWAY_TOKEN=<insert token>` and you're all aboard!", ui.RedText("RAILWAY_TOKEN environment variable not set."), ui.Bold("railway open"))
	EnvironmentNotFound                 RailwayError = fmt.Errorf("%s", ui.RedText("No active environment found. Please select one"))
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
)
