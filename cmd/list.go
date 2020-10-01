package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) List(ctx context.Context, req *entity.CommandRequest) error {
	projects, err := h.ctrl.GetProjects(ctx)
	if err != nil {
		return err
	}

	// TODO PRETTY
	for _, v := range projects {
		fmt.Println(v.Name)
	}

	return nil
}
