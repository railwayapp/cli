package cmd

import (
	"context"

	"github.com/railwayapp/cli/constants"
	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Docs(ctx context.Context, req *entity.CommandRequest) error {
	h.ctrl.ConfirmBrowserOpen("Opening Railway Docs...", constants.RailwayDocsURL)
	return nil
}
