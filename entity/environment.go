package entity

type Environment struct {
	Id   string `json:"id,omitempty"`
	Name string `json:"name,omitempty"`
}

type CreateEnvironmentRequest struct {
	Name      string `json:"name,omitempty"`
	ProjectID string `json:"projectId,omitempty"`
}
