package controller

import (
	"context"

	"github.com/railwayapp/cli/entity"
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
