package entity

import "bytes"

type UploadRequest struct {
	ProjectID     string
	EnvironmentID string
	ServiceID     string
	RootDir       string
}

type UpRequest struct {
	Data          bytes.Buffer
	ProjectID     string
	EnvironmentID string
	ServiceID     string
}

type UpResponse struct {
	URL              string
	DeploymentDomain string
}
