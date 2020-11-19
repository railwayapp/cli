package entity

type User struct {
	Id       string    `json:"id,omitempty"`
	Name     string    `json:"name,omitempty"`
	Email    string    `json:"email,omitempty"`
	Avatar   string    `json:"avatar,omitempty"`
	Projects []Project `json:"projects,omitempty"`
}
