package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Login(ctx context.Context, req *entity.CommandRequest) error {
	isBrowserless, err := req.Cmd.Flags().GetBool("browserless")
	if err != nil {
		return err
	}

	user, err := h.ctrl.Login(ctx, isBrowserless)
	if err != nil {
		return err
	}

	fmt.Printf("\n🎉 Logged in as %s (%s)\n", ui.Bold(user.Name), user.Email)

	return nil
}
