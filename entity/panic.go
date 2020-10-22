package entity

type PanicRequest struct {
	Command       string
	PanicError    string
	ProjectID     string
	EnvironmentID string
}
