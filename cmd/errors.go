package cmd

import (
	"context"
	"fmt"
)

func (h *Handler) Errors(ctx context.Context, i interface{}) error {
	// pass along error to controller that passes it to gateway to send it off to backboard
	fmt.Println("hey", i)
	//surpress errors
	return nil
}
