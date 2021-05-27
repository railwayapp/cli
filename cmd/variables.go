package cmd

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/railwayapp/cli/ui"

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
		if len(parts) != 2 {
			return errors.New("Invalid variables invokation. See --help")
		}
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

	err = h.redeployAfterVariablesChange(ctx, environment)
	if err != nil {
		return err
	}

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

	err = h.redeployAfterVariablesChange(ctx, environment)
	if err != nil {
		return err
	}

	return nil
}

func (h *Handler) redeployAfterVariablesChange(ctx context.Context, environment *entity.Environment) error {
	deployments, err := h.ctrl.GetDeployments(ctx)
	if err != nil {
		return err
	}

	// Don't redeploy if we don't yet have any deployments
	if len(deployments) == 0 {
		return nil
	}

	// Don't redeploy if the latest deploy for environment came from up
	latestDeploy := deployments[0]
	if latestDeploy.Meta == nil || latestDeploy.Meta.Repo == "" {
		fmt.Printf(ui.AlertInfo("Run %s to redeploy your project"), ui.MagentaText("railway up").Underline())
		return nil
	}

	ui.StartSpinner(&ui.SpinnerCfg{
		Message: fmt.Sprintf("Redeploying \"%s\" with new variables", environment.Name),
	})

	err = h.ctrl.DeployEnvironmentTriggers(ctx)
	if err != nil {
		return err
	}

	ui.StopSpinner("Deploy triggered")
	fmt.Printf("☁️ Deploy Logs available at %s\n", ui.GrayText(h.ctrl.GetProjectDeploymentsURL(ctx, latestDeploy.ProjectID)))
	return nil
}
