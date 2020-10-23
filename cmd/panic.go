package cmd

import (
	"context"
	"strings"

	"github.com/railwayapp/cli/errors"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Panic(ctx context.Context, panicErr string, stacktrace string, cmd string, args []string) error {
	cmd = cmd + " " + strings.Join(args, " ")
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
