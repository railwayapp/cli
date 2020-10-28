package controller

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (c *Controller) CreatePlugin(ctx context.Context, req *entity.CreatePluginRequest) (*entity.Plugin, error) {
	return c.gtwy.CreatePlugin(ctx, req)
}

func (c *Controller) GetAvailablePlugins(ctx context.Context, projectId string) (*[]string, error) {
	plugins, err := c.gtwy.GetAvailablePlugins(ctx, projectId)
	if err != nil {
		return nil, err
	}
	return plugins, nil
}
