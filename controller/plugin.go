package controller

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (c *Controller) CreatePlugin(ctx context.Context, req *entity.CreatePluginRequest) (*entity.Plugin, error) {
	return c.gtwy.CreatePlugin(ctx, req)
}

func (c *Controller) GetPlugins(ctx context.Context, projectId string) ([]*entity.Plugin, error) {
	projectCfg, err := c.gtwy.GetProject(ctx, projectId)
	if err != nil {
		return nil, err
	}
	plugins := projectCfg.Plugins
	return plugins, nil
}

func (c *Controller) PluginExists(ctx context.Context, pluginRequest string, projectId string) (bool, error) {
	plugins, err := c.GetPlugins(ctx, projectId)
	if err != nil {
		return false, err
	}
	doesExist := false
	for i := 0; i < len(plugins); i++ {
		if plugins[i].Name == pluginRequest {
			doesExist = true
		}
	}
	if doesExist {
		return false, nil
	}
	return true, nil
}
