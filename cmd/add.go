package cmd

import (
	"context"
	"fmt"
	"strings"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) addNew(ctx context.Context, req *entity.CommandRequest, pluginsAvailable []*entity.Plugin) (string, error) {
	pluginRequest, err := h.addNew(ctx, req)

	pluginResp, err := h.ctrl.CreatePlugin(ctx, &entity.CreatePluginRequest{
		ProjectID: project.Id,
		Plugin:    pluginRequest,
	})
	if err != nil {
		return err
	}
	fmt.Printf("🎉 Created plugin %s\n", ui.MagentaText(pluginResp.Name))
	return nil
	//fetch available plugins via controller
	fmt.Println("Plugins you can create:")
	pluginSelected, err := ui.PromptPlugins(pluginsAvailable)
	if err != nil {
		return "", err
	}
	return pluginSelected.Name, nil
}
func (h *Handler) addExists(ctx context.Context, req *entity.CommandRequest, pluginsAvailable []*entity.Plugin) (string, error) {
	pluginRequest, err := h.addNew(ctx, req)

	pluginResp, err := h.ctrl.CreatePlugin(ctx, &entity.CreatePluginRequest{
		ProjectID: project.Id,
		Plugin:    pluginRequest,
	})
	if err != nil {
		return err
	}
	fmt.Printf("🎉 Created plugin %s\n", ui.MagentaText(pluginResp.Name))
	return nil
	fmt.Println("You already created that plugin!\nPlugins you can create:")
	pluginSelected, err := ui.PromptPlugins(pluginsAvailable)
	if err != nil {
		return "", err
	}
	return pluginSelected.Name, nil
}
func (h *Handler) Add(ctx context.Context, req *entity.CommandRequest) error {
	projectCfg, err := h.cfg.GetProjectConfigs()
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return err
	}
	if len(req.Args[0]) == 0 {

	}
	pluginRequest := strings.TrimSpace(req.Args[0])
	allowCreation, pluginsAvailable, err := h.ctrl.PluginExists(ctx, pluginRequest, project.Id)
	if err != nil {
		return err
	}
	if !allowCreation {
		pluginRequest, err = h.addExists(ctx, req, pluginsAvailable)
	}

	pluginResp, err := h.ctrl.CreatePlugin(ctx, &entity.CreatePluginRequest{
		ProjectID: project.Id,
		Plugin:    pluginRequest,
	})
	if err != nil {
		return err
	}
	fmt.Printf("🎉 Created plugin %s\n", ui.MagentaText(pluginResp.Name))
	return nil
}
