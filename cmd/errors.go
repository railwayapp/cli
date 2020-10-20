package cmd

import (
	"context"
)

func (h *Handler) Errors(ctx context.Context, err error) error {
	// pass along error to controller that passes it to gateway to send it off to backboard
	h.ctrl.SendError(ctx, err)
	//surpress errors
	return nil
}
