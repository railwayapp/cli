package controller

import (
	"context"

	"github.com/google/go-github/github"
)

func (c *Controller) GetLatestVersion() (string, error) {
	client := github.NewClient(nil)
	rep, _, err := client.Repositories.GetLatestRelease(context.Background(), "railwayapp", "cli")
	return *rep.TagName, err
}
