package entity

type RootConfig struct {
	Token    string                     `json:"token"`
	Projects map[string][]ProjectConfig `json:"projects"`
}
