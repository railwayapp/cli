package cmd

import (
	"context"
	"errors"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func stringInSlice(a string, list []*string) bool {
    for _, b := range list {
        if *b == a {
            return true
        }
    }
    return false
}

func (h *Handler) addFromName(ctx context.Context, pluginRequest string, plugins []*string, projectId string) error {
	reqValid := stringInSlice(pluginRequest,plugins)
	if !reqValid {
		return errors.New("Plugin either has been created or not valid.")
	}
	plugin, err := h.ctrl.CreatePlugin(ctx, &entity.CreatePluginRequest{
		ProjectID: projectId,
		Plugin:    pluginRequest,
	})
	if err != nil {
		return err
	}
	fmt.Printf("🎉 Created plugin %s\n", ui.MagentaText(plugin.Name))
	return nil
}

func (h *Handler) addInit(ctx context.Context, plugins []*string, projectId string) error {
	selection, err := ui.PromptPlugins(plugins)
	if err != nil {
		return err
	}
	plugin, err := h.ctrl.CreatePlugin(ctx, &entity.CreatePluginRequest{
		ProjectID: projectId,
		Plugin:    *selection,
	})
	if err != nil {
		return err
	}
	fmt.Printf("🎉 Created plugin %s\n", ui.MagentaText(plugin.Name))
	return nil
}


func (h *Handler) Add(ctx context.Context, req *entity.CommandRequest) error {
	projectCfg, err := h.cfg.GetProjectConfigs()
	if err != nil {
		return err
	}
	plugins, err := h.ctrl.GetAvailablePlugins(ctx, projectCfg.Project)
	if err != nil {
		return err
	}
	if len(req.Args) > 0 {
		pluginRequest := req.Args[0]
		return h.addFromName(ctx, pluginRequest, plugins, projectCfg.Project)
	}
	return h.addInit(ctx, plugins, projectCfg.Project)
}
