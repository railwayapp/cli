package controller

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

const (
	RAILWAY_REPO_NAME  = "examples"
	RAILWAY_REPO_OWNER = "railwayapp"
)

func (c *Controller) GetRailwayTemplates(ctx context.Context, path string) ([]*entity.GithubFile, error) {
	return c.ghGateway.GetFilesInRepository(ctx, &entity.GithubFilesRequest{
		RepoName:  RAILWAY_REPO_NAME,
		RepoOwner: RAILWAY_REPO_OWNER,
		Path:      path,
	})
}
