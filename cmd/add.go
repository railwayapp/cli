package cmd

import (
	"context"
	"fmt"
	"strings"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Add(ctx context.Context, req *entity.CommandRequest) error {
	if len(req.Args) == 0 {
		return errors.PluginNotSpecified
	}
	projectCfg, err := h.cfg.GetProjectConfigs()
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return err
	}

	pluginRequest := strings.TrimSpace(req.Args[0])
	allowCreation, err := h.ctrl.PluginExists(ctx, pluginRequest, project.Id)
	if err != nil {
		return err
	}
	if !allowCreation {
		return errors.PluginAlreadyExists
	}
	createdPlugin, err := h.ctrl.CreatePlugin(ctx, &entity.CreatePluginRequest{
		ProjectID: project.Id,
		Plugin:    pluginRequest,
	})
	if err != nil {
		return err
	}
	fmt.Printf("🎉 Created plugin %s\n", ui.MagentaText(createdPlugin.Name))
	return nil
}
