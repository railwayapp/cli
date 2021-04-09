package entity

const (
	STATUS_BUILDING  = "BUILDING"
	STATUS_DEPLOYING = "DEPLOYING"
	STATUS_SUCCESS   = "SUCCESS"
	STATUS_REMOVED   = "REMOVED"
	STATUS_FAILED    = "FAILED"
)

type Deployment struct {
	ID         string `json:"id"`
	BuildLogs  string `json:"buildLogs"`
	DeployLogs string `json:"deployLogs"`
	Status     string `json:"status"`
}

type DeploymentLogsRequest struct {
	ProjectID    string `json:"projectId"`
	DeploymentID string `json:"deploymentId"`
	NumLines     int32  `json:"numLines"`
}

type DeploymentGQL struct {
	ID         bool `json:"id"`
	BuildLogs  bool `json:"buildLogs"`
	DeployLogs bool `json:"deployLogs"`
	Status     bool `json:"status"`
}

type DeploymentByIDRequest struct {
	ProjectID    string `json:"projectId"`
	DeploymentID string `json:"deploymentId"`
	GQL          DeploymentGQL
}
