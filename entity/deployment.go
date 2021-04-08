package entity

type Deployment struct {
	ID         string `json:"id"`
	BuildLogs  string `json:"buildLogs"`
	DeployLogs string `json:"deployLogs"`
}

type DeploymentLogsRequest struct {
	ProjectID    string `json:"projectId"`
	DeploymentID string `json:"deploymentId"`
	NumLines     int32  `json:"numLines"`
}
