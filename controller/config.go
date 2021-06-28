package controller

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (c *Controller) GetProjectConfigs(ctx context.Context) (*entity.ProjectConfig, error) {
	if c.cfg.RailwayProductionToken != "" {
		// Get project config from api
		projectToken, err := c.gtwy.GetProjectToken(ctx)
		if err != nil {
			return nil, err
		}

		if projectToken != nil {
			return &entity.ProjectConfig{
				Project:         projectToken.ProjectId,
				Environment:     projectToken.EnvironmentId,
				LockedEnvsNames: map[string]bool{},
			}, nil
		}
	}

	return c.cfg.GetProjectConfigs()
}

func (c *Controller) PromptIfProtectedEnvironment(ctx context.Context) error {
	projectCfg, err := c.GetProjectConfigs(ctx)
	if err != nil {
		return err
	}

	if val, ok := projectCfg.LockedEnvsNames[projectCfg.Environment]; ok && val {
		fmt.Println(ui.Bold(ui.RedText("Protected Environment Detected!").String()))
		confirm, err := ui.PromptYesNo("Continue?")
		if err != nil {
			return err
		}
		if !confirm {
			return nil
		}
	}

	return nil
}
