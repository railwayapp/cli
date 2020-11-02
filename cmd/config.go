package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Config(ctx context.Context, req *entity.CommandRequest) error {
	fmt.Println("configs")
	return nil
}
