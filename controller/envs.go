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

	fmt.Println(projectCfg.LockedEnvsNames)

	fmt.Println(projectCfg.Environment)

	if val, ok := projectCfg.LockedEnvsNames[projectCfg.Environment]; ok && val {
		fmt.Println(ui.Bold(ui.RedText("Protected Environment Detected!").BgBlack().String()))
		ui.PromptConfirm("Press Enter to Confirm Action")
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
