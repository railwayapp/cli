package controller

import (
	"context"
)

func (c *Controller) GetLatestVersion() (string, error) {
	rep, _, err := c.ghc.Repositories.GetLatestRelease(context.Background(), "railwayapp", "cli")
	if err != nil {
		return "", err
	}
	return *rep.TagName, nil
}
