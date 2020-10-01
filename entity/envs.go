package entity

type GetEnvsRequest struct {
	ProjectID     string
	EnvironmentID string
}

type Envs map[string]interface{}
