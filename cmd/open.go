package cmd

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Open(ctx context.Context, req *entity.CommandRequest) error {
	projectId, err := h.cfg.GetProject()
	if err != nil {
		return err
	}
	environmentId, err := h.cfg.GetCurrentEnvironment()
	if err != nil {
		return err
	}

	// If an unknown subcommand is used, show help
	if len(req.Args) > 0 {
		return req.Cmd.Help()
	}

	if req.Cmd.Use == "open" {
		return h.ctrl.OpenProjectInBrowser(ctx, projectId, environmentId)
	}

	return h.ctrl.OpenProjectPathInBrowser(ctx, projectId, environmentId, req.Cmd.Use)
}

func (h *Handler) OpenApp(ctx context.Context, req *entity.CommandRequest) error {
	projectId, err := h.cfg.GetProject()
	if err != nil {
		return err
	}
	environmentId, err := h.cfg.GetCurrentEnvironment()
	if err != nil {
		return err
	}

	deployment, err := h.ctrl.GetLatestDeploymentForEnvironment(ctx, projectId, environmentId)
	if err != nil {
		return err
	}

	return h.ctrl.OpenStaticUrlInBrowser(deployment.StaticUrl)
}
