package cmd

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Logout(ctx context.Context, req *entity.CommandRequest) error {
	return h.ctrl.Logout(ctx)
}
