package entity

type Environment struct {
	Id   string `json:"id,omitempty"`
	Name string `json:"name,omitempty"`
}

type CreateEnvironmentRequest struct {
	Name      string `json:"name,omitempty"`
	ProjectID string `json:"projectId,omitempty"`
}

type CreateEphemeralEnvironmentRequest struct {
	Name              string `json:"name,omitempty"`
	ProjectID         string `json:"projectId,omitempty"`
	BaseEnvironmentID string `json:"baseEnvironmentId"`
}

type DeleteEnvironmentRequest struct {
	EnvironmentId string `json:"environmentId,omitempty"`
	ProjectID     string `json:"projectId,omitempty"`
}
