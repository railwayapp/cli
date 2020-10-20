package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Login(ctx context.Context, req *entity.CommandRequest) error {
	user, err := h.ctrl.Login(ctx)
	if err != nil {
		return err
	}
	ui.StopSpinner(fmt.Sprintf("🎉 Logged in as %s (%s)", user.Name, user.Email))
	return nil
}
