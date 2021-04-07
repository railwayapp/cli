package entity

type Deployment struct {
	ID         string `json:"id"`
	BuildLogs  string `json:"buildLogs"`
	DeployLogs string `json:"deployLogs"`
	Status     string `json:"status"`
}
