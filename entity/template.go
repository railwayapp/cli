package entity

type TemplateEnvVar struct {
	Name         string `json:"name"`
	Desc         string `json:"desc"`
	DefaultValue string `json:"defaultValue"`
	Optional     bool   `json:"optional"`
}

type Template struct {
	Text    string           `json:"text"`
	Href    string           `json:"href"`
	Plugins []string         `json:"plugins"`
	Desc    string           `json:"desc"`
	Tags    []string         `json:"tags"`
	Icon    string           `json:"icon"`
	Value   string           `json:"value"`
	EnvVars []TemplateEnvVar `json:"envVars"`
}
