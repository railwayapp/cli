package cmd

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Protect(ctx context.Context, req *entity.CommandRequest) error {
	projectConfigs, err := h.ctrl.GetProjectConfigs(ctx)
	if err != nil {
		return err
	}

	mp := make(map[string]bool)

	for k, v := range projectConfigs.LockedEnvsNames {
		mp[k] = v
	}

	mp[projectConfigs.Environment] = true

	projectConfigs.LockedEnvsNames = mp

	err = h.cfg.SetProjectConfigs(projectConfigs)
	if err != nil {
		return err
	}

	return err
}
