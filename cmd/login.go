package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
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

	fmt.Printf("🎉 Logged in as %s (%s)", user.Name, user.Email)
	return nil
}
