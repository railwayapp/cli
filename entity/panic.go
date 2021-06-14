package entity

type PanicRequest struct {
	Command       string
	PanicError    string
	Stacktrace    string
	ProjectID     string
	EnvironmentID string
	Version       string
}
