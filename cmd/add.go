package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
)

// func (h *Handler) addInit(ctx context.Context, req *entity.CommandRequest, pluginsAvailable []*entity.Plugin) error {
// 	fmt.Println("Plugins you can create:")
// 	pluginSelected, err := ui.PromptPlugins(pluginsAvailable)
// 	pluginRequest = pluginSelected.Name
// 	if err != nil {
// 		return err
// 	}
// }

// func (h *Handler) addNew(ctx context.Context, plugin string) error {
// 	fmt.Println("Plugins you can create:")
// 	pluginSelected, err := ui.PromptPlugins(pluginsAvailable)
// 	pluginRequest = pluginSelected.Name
// 	if err != nil {
// 		return err
// 	}
// }

func (h *Handler) Add(ctx context.Context, req *entity.CommandRequest) error {

	projectID, err := h.cfg.GetProject()
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectID)
	if err != nil {
		return err
	}

	plugins, err := h.ctrl.GetAvailablePlugins(ctx, project.Id)
	if err != nil {
		return err
	}
	fmt.Println("PLUGINS", plugins)
	// if len(req.Args) > 0 {
	// 	// plugin provided as argument
	// 	plugin := req.Args[0]
	// 	return h.addNew(ctx, plugin)
	// }

	// isLoggedIn, _ := h.ctrl.IsLoggedIn(ctx)

	// if !isLoggedIn {
	// 	return errors.New("Account require to add plugins")
	// }

	// projectID, err := h.cfg.GetProject()
	// if err != nil {
	// 	return err
	// }

	// project, err := h.ctrl.GetProject(ctx, projectID)
	// if err != nil {
	// 	return err
	// }

	// plugins, err := h.ctrl.GetPlugins(ctx, pluginRequest, project.Id)
	// if err != nil {
	// 	return err
	// }

	// environment, err := ui.PromptPlugins(project.Environments)
	// if err != nil {
	// 	return err
	// }

	// err = h.cfg.SetEnvironment(environment.Id)
	// if err != nil {
	// 	return err
	// }

	// selection, err := ui.PromptPlugins(isLoggedIn)
	// if err != nil {
	// 	return err
	// }

	// projectCfg, err := h.cfg.GetProjectConfigs()
	// if err != nil {
	// 	return err
	// }

	// project, err := h.ctrl.GetProject(ctx, projectCfg.Project)
	// if err != nil {
	// 	return err
	// }

	// pluginRequest := strings.TrimSpace(req.Args[0])
	// allowCreation, pluginsAvailable, err := h.ctrl.PluginExists(ctx, pluginRequest, project.Id)
	// if err != nil {
	// 	return err
	// }
	// if !allowCreation {
	// 	fmt.Println("You already created that plugin!\nPlugins you can create:")
	// 	pluginSelected, err := ui.PromptPlugins(pluginsAvailable)
	// 	pluginRequest = pluginSelected.Name
	// 	if err != nil {
	// 		return err
	// 	}
	// }

	// pluginResp, err := h.ctrl.CreatePlugin(ctx, &entity.CreatePluginRequest{
	// 	ProjectID: project.Id,
	// 	Plugin:    pluginRequest,
	// })
	// if err != nil {
	// 	return err
	// }
	// fmt.Printf("🎉 Created plugin %s\n", ui.MagentaText(pluginResp.Name))
	return nil
}
