package errors

import "errors"

type RailwayError error

var (
	UserConfigNotFound      RailwayError = errors.New("Not logged in. Please run railway login.")
	ProjectConfigNotFound   RailwayError = errors.New("Not connected to a project. Run railway init to get started.")
	ProjectNotFound         RailwayError = errors.New("Project not found.")
	ProblemFetchingProjects RailwayError = errors.New("There was a problem fetching your projects")
	ProjectCreateFailed     RailwayError = errors.New("There was a problem creating the project")
	ProductionTokenNotSet   RailwayError = errors.New("RAILWAY_TOKEN environment variable not set")
	CommandNotSpecified     RailwayError = errors.New("Specify a command to run in side the railway environment. railway run <cmd>")
)
