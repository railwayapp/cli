package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
)

var Version = "master"

func (h *Handler) Version(ctx context.Context, req *entity.CommandRequest) error {
	version, err := h.ctrl.GetLatestVersion()
	if err != nil {
		return err
	}
	fmt.Println(fmt.Sprintf("railway version %s", version))
	return nil
}
