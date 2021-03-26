package controller

import (
	"context"
)

// GetWritableGithubScopes creates a project specified by the project request, error otherwise
func (c *Controller) GetWritableGithubScopes(ctx context.Context) ([]string, error) {
	return c.gtwy.GetWritableGithubScopes(ctx)
}
