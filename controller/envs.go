package controller

import (
	"context"
	"encoding/json"
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
		ui.PromptConfirm("Protected Environment!\n Confirm by typing the environment name!", projectCfg.Environment)
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
