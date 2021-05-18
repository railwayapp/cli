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
	environmentId, err := h.cfg.GetEnvironment()
	if err != nil {
		return err
	}

	if (req.Cmd.Use != "open") {
		return h.ctrl.OpenProjectPathInBrowser(ctx, projectId, environmentId, req.Cmd.Use)
	}

	return h.ctrl.OpenProjectInBrowser(ctx, projectId, environmentId)
}

func (h *Handler) OpenApp(ctx context.Context, req *entity.CommandRequest) error {
	projectId, err := h.cfg.GetProject()
	if err != nil {
		return err
	}
	environmentId, err := h.cfg.GetEnvironment()
	if err != nil {
		return err
	}

	deployment, err := h.ctrl.GetLatestDeploymentForEnvironment(ctx, projectId, environmentId)
	if err != nil {
		return err
	}

	return h.ctrl.OpenStaticUrlInBrowser(deployment.StaticUrl)
}
