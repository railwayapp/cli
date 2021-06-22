package entity

import "bytes"

type UpRequest struct {
	Data          bytes.Buffer
	ProjectID     string
	EnvironmentID string
}

type UpResponse struct {
	URL string
	DeploymentDomain string
}
