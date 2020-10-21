package controller

import (
	"context"
	"fmt"
	"os"

	"github.com/railwayapp/cli/ui"
)

// GetProject returns a project of id projectId, error otherwise
func (c *Controller) SendPanic(ctx context.Context, i interface{}) error {
	confirmSendPanic()
	return c.gtwy.SendPanic(ctx, i)
}

func confirmSendPanic() {
	fmt.Printf("🚨 Looks like something derailed, Press Enter to send error logs (^C to quit)")
	fmt.Fscanln(os.Stdin)
	ui.StartSpinner(&ui.SpinnerCfg{
		Message: "Taking notes...",
	})
}
