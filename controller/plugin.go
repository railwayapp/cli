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
	return projectCfg.Plugins, nil
}

func availablePlugins(pluginRequest string) []*entity.Plugin {
	set := map[string]bool{"env": true, "postgresql": true, "mongodb": true, "redis": true}
	delete(set, pluginRequest)
	keys := []*entity.Plugin{}
	for key, _ := range set {
		keys = append(keys, &entity.Plugin{Name: key})
	}
	return keys
}
func (c *Controller) PluginExists(ctx context.Context, pluginRequest string, projectId string) (bool, []*entity.Plugin, error) {
	plugins, err := c.GetPlugins(ctx, projectId)
	if err != nil {
		return true, nil, err
	}
	allowCreation := true
	for i := 0; i < len(plugins); i++ {
		if plugins[i].Name == pluginRequest {
			allowCreation = false
		}
	}
	if !allowCreation {
		return false, availablePlugins(pluginRequest), nil
	}
	return true, availablePlugins(""), nil
}
