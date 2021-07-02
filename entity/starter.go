package entity

type Starter struct {
	Title  string `json:"title"`
	Url    string `json:"url"`
	Source string `json:"source"`
}

type StarterEnvVar struct {
	Name     string
	Desc     string
	Default  string
	Optional bool
}
