package controller

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (c *Controller) GetPlugins(ctx context.Context, projectId string) ([]*entity.Plugin, error) {
	return c.gtwy.GetPlugins(ctx, projectId)
}
