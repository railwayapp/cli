package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/constants"
	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Version(ctx context.Context, req *entity.CommandRequest) error {
	fmt.Println(fmt.Sprintf("railway version %s", constants.Version))
	return nil
}

func (h *Handler) CheckVersion(ctx context.Context, req *entity.CommandRequest) error {
	if constants.Version != "Piped into LDflags on build. You are probably running Railway CLI from source." {
		latest, _ := h.ctrl.GetLatestVersion()
		// Surpressing error as getting latest version is desired, not required

		if latest != "" && latest != constants.Version {
			fmt.Printf("A newer version of the Railway CLI is available, please update to: %s\n", latest)
		}
	}
	return nil
}
