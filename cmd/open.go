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

	h.ctrl.OpenProjectInBrowser(ctx, projectId)
	return nil
}
