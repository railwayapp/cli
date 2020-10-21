package cmd

import (
	"context"
	"fmt"
)

func (h *Handler) Errors(ctx context.Context, err error) error {
	// pass along error to controller that passes it to gateway to send it off to backboard
	fmt.Println("hey")
	//surpress errors
	return nil
}
