package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
)

func Min(x, y int) int {
	if x < y {
		return x
	}
	return y
}

func Max(x, y int) int {
	if x > y {
		return x
	}
	return y
}

func (h *Handler) Variables(ctx context.Context, req *entity.CommandRequest) error {
	envs, err := h.ctrl.GetEnvs(ctx)
	if err != nil {
		return err
	}

	var minSpacing = 15
	var maxSpacing = 100
	var longest = 0

	for k := range *envs {
		if len(k) > longest {
			longest = len(k)
		}
	}

	for k, v := range *envs {
		fmt.Printf("%-*s\t%s\n", Max(minSpacing, Min(maxSpacing, longest)), k, v)
	}
	return nil
}

func (h *Handler) EnvSet(ctx context.Context, req *entity.CommandRequest) error {
	return nil
}

func (h *Handler) EnvGet(ctx context.Context, req *entity.CommandRequest) error {
	return nil
}

func (h *Handler) EnvDelete(ctx context.Context, req *entity.CommandRequest) error {
	return nil
}
