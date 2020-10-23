package entity

type PluginList struct {
	Plugins []*Plugin `json:"plugins,omitempty"`
}

type Plugin struct {
	Id   string `json:"id,omitempty"`
	Name string `json:"name,omitempty"`
}

type CreatePluginRequest struct {
	ProjectID string
	Plugin    string
}
