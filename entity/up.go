package entity

import (
	"bytes"

	"github.com/railwayapp/cli/lib/git"
)

type UploadRequest struct {
	ProjectID     string
	EnvironmentID string
	ServiceID     string
	RootDir       string
	GitInfo       git.GitMetadata
}

type UpRequest struct {
	Data          bytes.Buffer
	ProjectID     string
	EnvironmentID string
	ServiceID     string
	GitInfo       git.GitMetadata
}

type UpResponse struct {
	URL              string
	DeploymentDomain string
}

type UpErrorResponse struct {
	Message   string `json:"message"`
	RequestID string `json:"reqId"`
}
