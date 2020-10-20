package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Env(ctx context.Context, req *entity.CommandRequest) error {
	envs, err := h.ctrl.GetEnvs(ctx)
	if err != nil {
		return err
	}

	for k, v := range *envs {
		fmt.Printf("%-15s%-15s\n", k, v)
	}
	return nil
}
