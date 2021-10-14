package cmd

import (
	"context"
	"fmt"
	"strings"

	"github.com/railwayapp/cli/errors"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Panic(ctx context.Context, panicErr, stacktrace, cmd string, args []string) error {
	cmd = cmd + " " + strings.Join(args, " ")
	for _, arg := range args {
		if arg == "-v" {
			// Verbose mode show err
			fmt.Println(panicErr, stacktrace)
		}
	}

	success, err := h.ctrl.SendPanic(ctx, panicErr, stacktrace, cmd)
	if err != nil {
		return err
	}
	if success {
		ui.StopSpinner("Successfully sent the error! We're figuring out what went wrong.")
		return nil
	}
	return errors.TelemetryFailed
}
