package cmd

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Logs(ctx context.Context, req *entity.CommandRequest) error {
	detach, err := req.Cmd.Flags().GetBool("detach")
	if err != nil {
		return err
	}
	return h.fetchLogs(ctx, detach)
}

func (h *Handler) fetchLogs(ctx context.Context, detached bool) error {
	// Nonstreaming
	if detached {
		deployLogs, err := h.ctrl.GetActiveDeploymentLogs(ctx)
		if err != nil {
			return err
		}
		fmt.Println(deployLogs)
		return nil
	}
	// Streaming
	prevIdx := 0
	for {
		err := func() error {
			defer time.Sleep(2 * time.Second)
			deployLogs, err := h.ctrl.GetActiveDeploymentLogs(ctx)
			if err != nil {
				return err
			}
			partials := strings.Split(deployLogs, "\n")
			nextIdx := len(partials)
			delta := partials[prevIdx:nextIdx]
			if len(delta) == 0 {
				return nil
			}
			fmt.Println(strings.Join(delta, "\n"))
			prevIdx = nextIdx
			return nil
		}()
		if err != nil {
			return err
		}
	}
}
