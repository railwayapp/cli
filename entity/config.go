package entity

type RootConfig struct {
	User     UserConfig               `json:"user"`
	Projects map[string]ProjectConfig `json:"projects"`
}

type UserConfig struct {
	Token string `json:"token"`
}

type ProjectConfig struct {
	ProjectPath string `json:"projectPath,omitempty"`
	Project     string `json:"project,omitempty"`
	Environment string `json:"environment,omitempty"`
}
