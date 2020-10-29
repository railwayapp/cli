package entity

type RootConfig struct {
	Token    string          `json:"token"`
	Projects []ProjectConfig `json:"projects"`
}
