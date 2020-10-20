package errors

import (
	"errors"
	"fmt"

	"github.com/railwayapp/cli/ui"
)

type RailwayError error

var (
	UserConfigNotFound      RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("Not logged in. Run railway login.")))
	ProjectConfigNotFound   RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("Not connected to a project. Run railway init to get started.")))
	ProjectNotFound         RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("Project not found.\nTry railway init to get plugged into a new or existing project.")))
	ProblemFetchingProjects RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("There was a problem fetching your projects.\nOne of our trains probably derailed!")))
	ProjectCreateFailed     RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("There was a problem creating the project.\nOne of our trains probably derailed!")))
	ProductionTokenNotSet   RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("RAILWAY_TOKEN environment variable not set.\nRun railway open project and head under `tokens` section. You can generate tokens to access Railway environment variables. Set that token in your environment as `RAILWAY_TOKEN=<insert token>` and you're all aboard!")))
	CommandNotSpecified     RailwayError = errors.New(fmt.Sprintf("Specify a command to run in side the railway environment.\n%s %s", ui.Bold("railway run"), ui.MagentaText("<cmd>")))
)
