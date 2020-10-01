package errors

import "errors"

type RailwayError error

var (
	UserConfigNotFound    RailwayError = errors.New("Not logged in. Please run railway login.")
	ProjectConfigNotFound RailwayError = errors.New("Not connected to a project. Run railway init to get started.")
	ProjectNotFound       RailwayError = errors.New("Project not found.")
	ProjectCreateFailed   RailwayError = errors.New("There was a problem creating the project")
)
