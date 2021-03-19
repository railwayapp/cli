package cmd

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Protect(ctx context.Context, req *entity.CommandRequest) error {
	projectConfigs, err := h.cfg.GetProjectConfigs()
	if err != nil {
		return err
	}
	projectConfigs.LockedEnvsNames[projectConfigs.Environment] = true

	err = h.cfg.SetProjectConfigs(projectConfigs)
	if err != nil {
		return err
	}

	return err
}
