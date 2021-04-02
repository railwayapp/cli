package cmd

import (
	"context"
	"fmt"
	"github.com/railwayapp/cli/ui"
	"strings"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Variables(ctx context.Context, _ *entity.CommandRequest) error {
	envs, err := h.ctrl.GetEnvs(ctx)
	if err != nil {
		return err
	}

	environment, err := h.ctrl.GetEnvironment(ctx)
	if err != nil {
		return err
	}

	fmt.Print(ui.Heading(fmt.Sprintf("%s Environment Variables", environment.Name)))
	fmt.Print(ui.KeyValues(*envs))

	return nil
}

func (h *Handler) VariablesGet(ctx context.Context, req *entity.CommandRequest) error {
	envs, err := h.ctrl.GetEnvs(ctx)
	if err != nil {
		return err
	}

	for _, key := range req.Args {
		fmt.Println(envs.Get(key))
	}

	return nil
}

func (h *Handler) VariablesSet(ctx context.Context, req *entity.CommandRequest) error {
	envs, err := h.ctrl.GetEnvsForEnvPlugin(ctx)
	if err != nil {
		return err
	}

	updatedEnvs := &entity.Envs{}
	updatedEnvNames := make([]string, 0)
	for _, kvPair := range req.Args {
		parts := strings.SplitN(kvPair, "=", 2)
		key := parts[0]
		value := parts[1]

		envs.Set(key, value)
		updatedEnvs.Set(key, value)
		updatedEnvNames = append(updatedEnvNames, key)
	}

	_, err = h.ctrl.UpdateEnvsForEnvPlugin(ctx, envs)
	if err != nil {
		return err
	}

	environment, err := h.ctrl.GetEnvironment(ctx)
	if err != nil {
		return err
	}

	fmt.Print(ui.Heading(fmt.Sprintf("Updated %s for \"%s\"", strings.Join(updatedEnvNames, ", "), environment.Name)))
	fmt.Print(ui.KeyValues(*updatedEnvs))

	return nil
}

func (h *Handler) VariablesDelete(ctx context.Context, req *entity.CommandRequest) error {
	envs, err := h.ctrl.GetEnvsForEnvPlugin(ctx)
	if err != nil {
		return err
	}

	updatedEnvNames := make([]string, 0)
	for _, key := range req.Args {
		updatedEnvNames = append(updatedEnvNames, key)
		envs.Delete(key)
	}

	_, err = h.ctrl.UpdateEnvsForEnvPlugin(ctx, envs)
	if err != nil {
		return err
	}

	environment, err := h.ctrl.GetEnvironment(ctx)
	if err != nil {
		return err
	}

	fmt.Print(ui.Heading(fmt.Sprintf("Deleted %s for \"%s\"", strings.Join(updatedEnvNames, ", "), environment.Name)))

	return nil
}
