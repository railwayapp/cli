package entity

type PanicRequest struct {
	Msg           interface{}
	ProjectID     string
	EnvironmentID string
}
