package errors

import "errors"

type RailwayError error

var (
	UserConfigNotFound      RailwayError = errors.New("Not logged in. Run railway login.")
	ProjectConfigNotFound   RailwayError = errors.New("Not connected to a project. Run railway init to get started.")
	ProjectNotFound         RailwayError = errors.New("Project not found.\nTry railway init to get plugged into a new or existing project.")
	ProblemFetchingProjects RailwayError = errors.New("There was a problem fetching your projects.\nOne of our trains probably derailed!")
	ProjectCreateFailed     RailwayError = errors.New("There was a problem creating the project\nOne of our trains probably derailed!")
	ProductionTokenNotSet   RailwayError = errors.New("RAILWAY_TOKEN environment variable not set.\nRun railway open project and head under `tokens` section. You can generate tokens to access Railway environment variables. Set that token in your environment as `RAILWAY_TOKEN=<insert token>` and you're all aboard!")
	CommandNotSpecified     RailwayError = errors.New("Specify a command to run in side the railway environment. railway run <cmd>")
)
