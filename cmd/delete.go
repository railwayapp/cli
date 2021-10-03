package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
	"github.com/railwayapp/cli/uuid"
)

func (h *Handler) Delete(ctx context.Context, req *entity.CommandRequest) error {
	if len(req.Args) > 0 {
		// projectID provided as argument
		arg := req.Args[0]

		if uuid.IsValidUUID(arg) {
			project, err := h.ctrl.GetProject(ctx, arg)
			if err != nil {
				return err
			}

			return h.ctrl.DeleteProject(ctx, project.Id)
		}

		project, err := h.ctrl.GetProjectByName(ctx, arg)
		if err != nil {
			return err
		}

		return h.ctrl.DeleteProject(ctx, project.Id)
	}

	h.Unlink(ctx, req)

	isLoggedIn, err := h.ctrl.IsLoggedIn(ctx)
	if err != nil {
		return err
	}

	if isLoggedIn {
		return h.deleteFromAccount(ctx, req)
	} else {
		return h.deleteFromID(ctx, req)
	}

}

func (h *Handler) deleteFromAccount(ctx context.Context, req *entity.CommandRequest) error {
	projects, err := h.ctrl.GetProjects(ctx)
	if err != nil {
		return err
	}

	if len(projects) == 0 {
		fmt.Printf("No Projects could be deleted.")
		return nil
	}

	project, err := ui.PromptProjects(projects)
	if err != nil {
		return err
	}
	print("Looks good")

	return h.ctrl.DeleteProject(ctx, project.Id)
}

func (h *Handler) deleteFromID(ctx context.Context, req *entity.CommandRequest) error {
	projectID, err := ui.PromptText("Enter your project id")
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectID)
	print("Looks good")

	if err != nil {
		return err
	}

	return h.ctrl.DeleteProject(ctx, project.Id)
}
