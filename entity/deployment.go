package entity

type Deployment struct {
	BuildLogs  string `json:"buildLogs"`
	DeployLogs string `json:"deployLogs"`
}
