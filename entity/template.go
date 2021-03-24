package entity

type Template struct {
	Text    string   `json:"text"`
	Href    string   `json:"href"`
	Plugins []string `json:"plugins"`
	Desc    string   `json:"desc"`
	Tags    []string `json:"tags"`
	Icon    string   `json:"icon"`
	Value   string   `json:"value"`
	EnvVars []struct {
		Name string `json:"name"`
		Desc string `json:"desc"`
	} `json:"envVars"`
}
