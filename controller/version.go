package controller

import (
	"context"
)

func (c *Controller) GetLatestVersion() (string, error) {
	rep, _, err := c.ghc.Repositories.GetLatestRelease(context.Background(), "railwayapp", "cli")
	return *rep.TagName, err
}
