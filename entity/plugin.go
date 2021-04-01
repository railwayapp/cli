package entity

type PluginList struct {
	Plugins []*Plugin `json:"plugins,omitempty"`
}

type Plugin struct {
	ID   string `json:"id,omitempty"`
	Name string `json:"name,omitempty"`
}

type CreatePluginRequest struct {
	ProjectID string
	Plugin    string
}
