package entity

type CreateProjectRequest struct {
	Name        *string  // Optional
	Description *string  // Optional
	Plugins     []string // Optional
}

type UpdateProjectRequest struct {
	Id          string  // Required
	Name        *string // Optional
	Description *string // Optional
}

type Project struct {
	Id           string         `json:"id,omitempty"`
	Name         string         `json:"name,omitempty"`
	Environments []*Environment `json:"environments,omitempty"`
	Plugins      []*Plugin      `json:"plugins,omitempty"`
}

type ProjectConfig struct {
	Project     string `json:"projectId,omitempty"`
	Environment string `json:"environmentId,omitempty"`
	Path        string `json:"path,omitempty"`
}
