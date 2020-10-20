package errors

import (
	"errors"
	"fmt"

	"github.com/railwayapp/cli/ui"
)

type RailwayError error

var (
	UserConfigNotFound      RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("Not logged in. Please run railway login.")))
	ProjectConfigNotFound   RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("Not connected to a project. Run railway init to get started.")))
	ProjectNotFound         RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("Project not found.")))
	ProblemFetchingProjects RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("There was a problem fetching your projects")))
	ProjectCreateFailed     RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("There was a problem creating the project")))
	ProductionTokenNotSet   RailwayError = errors.New(fmt.Sprintf("%s", ui.RedText("RAILWAY_TOKEN environment variable not set")))
	CommandNotSpecified     RailwayError = errors.New(fmt.Sprintf("Specify a command to run in side the railway environment.\n%s %s", ui.Bold("railway run"), ui.MagentaText("<cmd>")))
)
