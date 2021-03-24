package errors

import (
	"errors"
	"fmt"

	"github.com/railwayapp/cli/ui"
)

type RailwayError error

var (
	UserConfigNotFound                  RailwayError = errors.New(fmt.Sprintf("%s\nRun %s", ui.RedText("Not logged in."), ui.Bold("railway login")))
	ProjectConfigNotFound               RailwayError = errors.New(fmt.Sprintf("%s. Tip: If you haven't, do railway login\nOtherwise, run %s to get plugged into a new or existing project.", ui.RedText("Project not found."), ui.Bold("railway init")))
	ProblemFetchingProjects             RailwayError = errors.New(fmt.Sprintf("%s\nOne of our trains probably derailed!", ui.RedText("There was a problem fetching your projects.")))
	ProblemFetchingWritableGithubScopes RailwayError = errors.New(fmt.Sprintf("%s\nOne of our trains probably derailed!", ui.RedText("There was a problem fetching GitHub metadata.")))
	ProjectCreateFailed                 RailwayError = errors.New(fmt.Sprintf("%s\nOne of our trains probably derailed!", ui.RedText("There was a problem creating the project.")))
	ProjectCreateFromTemplateFailed     RailwayError = errors.New(fmt.Sprintf("%s\nOne of our trains probably derailed!", ui.RedText("There was a problem creating the project from template.")))
	ProductionTokenNotSet               RailwayError = errors.New(fmt.Sprintf("%s\nRun %s and head under `tokens` section. You can generate tokens to access Railway environment variables. Set that token in your environment as `RAILWAY_TOKEN=<insert token>` and you're all aboard!", ui.RedText("RAILWAY_TOKEN environment variable not set."), ui.Bold("railway open")))
	NoGitHubScopesFound                 RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("No GitHub organizations found. Please link your GitHub account to Railway and try again.")))
	CommandNotSpecified                 RailwayError = errors.New(fmt.Sprintf("%s\nRun %s", ui.RedText("Specify a command to run inside the railway environment. Not providing a command will build and run the Dockerfile in the current directory."), ui.Bold("railway run [cmd]")))
	LoginFailed                         RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("Login failed")))
	LoginTimeout                        RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("Login timeout")))
	PluginAlreadyExists                 RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("Plugin already exists")))
	PluginNotSpecified                  RailwayError = errors.New(fmt.Sprintf("%s\nRun %s", ui.RedText("Specify a plugin to create."), ui.Bold("railway add <plugin>")))
	PluginCreateFailed                  RailwayError = errors.New(fmt.Sprintf("%s\nUhh Ohh! One of our trains derailed.", ui.RedText("There was a problem creating the plugin.")))
	PluginGetFailed                     RailwayError = errors.New(fmt.Sprintf("%s\nUhh Ohh! One of our trains derailed.", ui.RedText("There was a problem getting plugins available for creation.")))
	TelemetryFailed                     RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("One of our trains derailed. Any chance you can report this error on our Discord (https://railway.app/help)?")))
	WorkflowFailed                      RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("There was a problem deploying the project. Any chance you can report this error on our Discord (https://railway.app/help)?")))
)
