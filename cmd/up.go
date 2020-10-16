package cmd

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Up(ctx context.Context, req *entity.CommandRequest) error {
	return h.ctrl.Up(ctx)
}
