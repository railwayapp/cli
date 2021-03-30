package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Whoami(ctx context.Context, req *entity.CommandRequest) error {
	user, err := h.ctrl.GetUser(ctx)
	if err != nil {
		return err
	}

	userText := fmt.Sprintf("%s", ui.MagentaText(user.Email))
	if user.Name != "" {
		userText = fmt.Sprintf("%s (%s)", user.Name, ui.MagentaText(user.Email))
	}
	fmt.Printf("👋 Hey %s\n", userText)

	// Todo, more info, also more fun
	return nil
}
