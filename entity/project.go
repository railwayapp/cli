package entity

type CreateProjectRequest struct {
	Name        *string  // Optional
	Description *string  // Optional
	Plugins     []string // Optional
}

type CreateProjectFromTemplateRequest struct {
	Name      string            // Required
	Owner     string            // Required
	Template  string            // Required
	IsPrivate bool              // Optional
	Plugins   []string          // Optional
	Variables map[string]string // Optional
}

type UpdateProjectRequest struct {
	Id          string  // Required
	Name        *string // Optional
	Description *string // Optional
}

type CreateProjectFromTemplateResult struct {
	WorkflowID string
	ProjectID  string
}

type Project struct {
	Id           string         `json:"id,omitempty"`
	Name         string         `json:"name,omitempty"`
	UpdatedAt    string         `json:"updatedAt,omitempty"`
	Environments []*Environment `json:"environments,omitempty"`
	Plugins      []*Plugin      `json:"plugins,omitempty"`
	Team         *string        `json:"team,omitempty"`
	Services     []*Service     `json:"services,omitempty"`
}

type ProjectToken struct {
	ProjectId     string `json:"projectId"`
	EnvironmentId string `json:"environmentId"`
}
