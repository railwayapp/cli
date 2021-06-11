package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
	"github.com/railwayapp/cli/uuid"
)

func (h *Handler) Link(ctx context.Context, req *entity.CommandRequest) error {
	if len(req.Args) > 0 {
		// projectID provided as argument
		arg := req.Args[0]

		if (uuid.IsValidUUID(arg)) {
			project, err := h.ctrl.GetProject(ctx, arg)
			if (err != nil) {
				return err
			}

			return h.setProject(ctx, project)
		}

		project, err := h.ctrl.GetProjectByName(ctx, arg)
		if (err != nil) {
			return err
		}

		return h.setProject(ctx, project)
	}

	isLoggedIn, err := h.ctrl.IsLoggedIn(ctx)
	if err != nil {
		return err
	}

	if isLoggedIn {
		return h.linkFromAccount(ctx, req)
	} else {
		return h.linkFromID(ctx, req)
	}
}

func (h *Handler) linkFromAccount(ctx context.Context, req *entity.CommandRequest) error {
	projects, err := h.ctrl.GetProjects(ctx)
	if err != nil {
		return err
	}

	if len(projects) == 0 {
		fmt.Printf("No Projects. Create one with %s\n", ui.GreenText("railway init"))
		return nil
	}

	project, err := ui.PromptProjects(projects)
	if err != nil {
		return err
	}

	return h.setProject(ctx, project)
}

func (h *Handler) linkFromID(ctx context.Context, req *entity.CommandRequest) error {
	projectID, err := ui.PromptText("Enter your project id")
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectID)
	if (err != nil) {
		return err
	}

	return h.setProject(ctx, project)
}
