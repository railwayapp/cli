package controller

import (
	"context"
)

// GetProject returns a project of id projectId, error otherwise
func (c *Controller) SendErrors(ctx context.Context, err error) error {
	return c.gtwy.SendErrors(ctx, err)
}
