package cmd

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Logs(ctx context.Context, req *entity.CommandRequest) error {
	isLoggedIn, _ := h.ctrl.IsLoggedIn(ctx)

	if !isLoggedIn {
		return fmt.Errorf("%s\nRun %s", ui.RedText("Account require to init project"), ui.Bold("railway login"))
	}

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
