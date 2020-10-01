package controller

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (c *Controller) GetEnvs(ctx context.Context) (*entity.Envs, error) {
	projectCfg, err := c.cfg.GetProjectConfigs()
	if err != nil {
		return nil, err
	}
	return c.gtwy.GetEnvs(ctx, &entity.GetEnvsRequest{
		ProjectID:     projectCfg.Project,
		EnvironmentID: projectCfg.Environment,
	})
}
