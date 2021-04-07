package controller

import (
	"context"
	"encoding/json"
	"fmt"
	"io/ioutil"
	"os"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (c *Controller) GetEnvs(ctx context.Context) (*entity.Envs, error) {
	// Get envs through production token if it exists
	if c.cfg.RailwayProductionToken != "" {
		envs, err := c.gtwy.GetEnvsWithProjectToken(ctx)
		if err != nil {
			return nil, err
		}

		return envs, err
	}

	projectCfg, err := c.cfg.GetProjectConfigs()
	if err != nil {
		return nil, err
	}

	if val, ok := projectCfg.LockedEnvsNames[projectCfg.Environment]; ok && val {
		fmt.Println(ui.Bold(ui.RedText("Protected Environment Detected!").String()))
		confirm, err := ui.PromptYesNo("Continue fetching variables?")
		if err != nil {
			return nil, err
		}
		if !confirm {
			return nil, nil
		}
	}

	return c.gtwy.GetEnvs(ctx, &entity.GetEnvsRequest{
		ProjectID:     projectCfg.Project,
		EnvironmentID: projectCfg.Environment,
	})
}

func (c *Controller) SaveEnvsToFile(ctx context.Context) error {
	envs, err := c.GetEnvs(ctx)
	if err != nil {
		return err
	}

	err = c.cfg.CreatePathIfNotExist(c.cfg.RailwayEnvFilePath)
	if err != nil {
		return err
	}

	encoded, err := json.MarshalIndent(envs, "", "  ")
	if err != nil {
		return err
	}

	err = ioutil.WriteFile(c.cfg.RailwayEnvFilePath, encoded, os.ModePerm)
	if err != nil {
		return err
	}

	return nil
}

func (c *Controller) UpdateEnvsForEnvPlugin(ctx context.Context, envs *entity.Envs) (*entity.Envs, error) {
	projectCfg, err := c.cfg.GetProjectConfigs()
	if err != nil {
		return nil, err
	}
	if val, ok := projectCfg.LockedEnvsNames[projectCfg.Environment]; ok && val {
		fmt.Println(ui.Bold(ui.RedText("Protected Environment Detected!").String()))
		confirm, err := ui.PromptYesNo("Continue updating variables?")
		if err != nil {
			return nil, err
		}
		if !confirm {
			return nil, nil
		}
	}

	project, err := c.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return nil, err
	}

	pluginID := ""
	for _, p := range project.Plugins {
		if p.Name == "env" {
			pluginID = p.ID
		}
	}

	environment, err := c.cfg.GetEnvironment()
	if err != nil {
		return nil, err
	}

	return c.gtwy.UpdateEnvsForPlugin(ctx, &entity.UpdateEnvsRequest{
		ProjectID:     projectCfg.Project,
		EnvironmentID: environment,
		PluginID:      pluginID,
		Envs:          envs,
	})
}

func (c *Controller) GetEnvsForEnvPlugin(ctx context.Context) (*entity.Envs, error) {
	// Get envs through project token if it exists
	if c.cfg.RailwayProductionToken != "" {
		envs, err := c.gtwy.GetEnvsWithProjectToken(ctx)
		if err != nil {
			return nil, err
		}

		return envs, err
	}

	projectCfg, err := c.cfg.GetProjectConfigs()
	if err != nil {
		return nil, err
	}

	project, err := c.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return nil, err
	}

	pluginID := ""
	for _, p := range project.Plugins {
		if p.Name == "env" {
			pluginID = p.ID
		}
	}

	if val, ok := projectCfg.LockedEnvsNames[projectCfg.Environment]; ok && val {
		fmt.Println(ui.Bold(ui.RedText("Protected Environment Detected!").String()))
		confirm, err := ui.PromptYesNo("Continue fetching variables?")
		if err != nil {
			return nil, err
		}
		if !confirm {
			return nil, nil
		}
	}

	return c.gtwy.GetEnvsForPlugin(ctx, &entity.GetEnvsForPluginRequest{
		ProjectID:     projectCfg.Project,
		EnvironmentID: projectCfg.Environment,
		PluginID:      pluginID,
	})
}
