package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Add(ctx context.Context, req *entity.CommandRequest) error {
	projectCfg, err := h.ctrl.GetProjectConfigs(ctx)
	if err != nil {
		return err
	}
	plugins, err := h.ctrl.GetAvailablePlugins(ctx, projectCfg.Project)
	if err != nil {
		return err
	}
	selectedPlugin, err := ui.PromptPlugins(plugins)
	if err != nil {
		return err
	}
	ui.StartSpinner(&ui.SpinnerCfg{
		Message: fmt.Sprintf("Adding %s plugin", selectedPlugin),
	})
	defer ui.StopSpinner("")

	plugin, err := h.ctrl.CreatePlugin(ctx, &entity.CreatePluginRequest{
		ProjectID: projectCfg.Project,
		Plugin:    selectedPlugin,
	})
	if err != nil {
		return err
	}
	fmt.Printf("🎉 Created plugin %s\n", ui.MagentaText(plugin.Name))
	return nil

}
