package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/constants"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Version(ctx context.Context, req *entity.CommandRequest) error {
	fmt.Printf("railway version %s", ui.MagentaText(constants.Version))
	return nil
}

func (h *Handler) CheckVersion(ctx context.Context, req *entity.CommandRequest) error {
	if constants.Version != constants.VersionDefault {
		latest, _ := h.ctrl.GetLatestVersion()
		// Suppressing error as getting latest version is desired, not required
		if latest != "" && latest[1:] != constants.Version {
			fmt.Println(ui.Bold(fmt.Sprintf("A newer version of the Railway CLI is available, please update to: %s", ui.MagentaText(latest))))
		}
	}
	return nil
}
