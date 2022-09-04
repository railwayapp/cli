package controller

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (c *Controller) DeployEnvironmentTriggers(ctx context.Context, serviceID *string) error {
	projectCfg, err := c.GetProjectConfigs(ctx)
	if err != nil {
		return err
	}

	return c.gtwy.DeployEnvironmentTriggers(ctx, &entity.DeployEnvironmentTriggersRequest{
		ProjectID:     projectCfg.Project,
		EnvironmentID: projectCfg.Environment,
		ServiceID:     *serviceID,
	})
}
