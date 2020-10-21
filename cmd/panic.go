package cmd

import (
	"context"
)

func (h *Handler) Panic(ctx context.Context, i interface{}) error {
	// pass along error to controller that passes it to gateway to send it off to backboard
	h.ctrl.SendPanic(ctx, i)
	//surpress errors
	return nil
}
