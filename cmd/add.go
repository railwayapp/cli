package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
	"github.com/railwayapp/cli/ui"
)

func stringInSlice(a string, list []string) bool {
	for _, b := range list {
		if b == a {
			return true
		}
	}
	return false
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

	selection := ""

	if len(req.Args) > 0 {
		selection = req.Args[0]
	} else {
		selection, err = ui.PromptPlugins(plugins)
		if err != nil {
			return err
		}
	}

	if !stringInSlice(selection, plugins) {
		return errors.PluginNotFound
	}

	plugin, err := h.ctrl.CreatePlugin(ctx, &entity.CreatePluginRequest{
		ProjectID: projectCfg.Project,
		Plugin:    selection,
	})
	if err != nil {
		return err
	}

	fmt.Printf("🎉 Created plugin %s\n", ui.MagentaText(plugin.Name))
	return nil

}
