package controller

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

func (c *Controller) GetDeployments(ctx context.Context) ([]*entity.Deployment, error) {
	projectConfig, err := c.GetProjectConfigs(ctx)
	if err != nil {
		return nil, err
	}

	return c.gtwy.GetDeploymentsForEnvironment(ctx, projectConfig.Project, projectConfig.Environment)
}

func (c *Controller) GetActiveDeployment(ctx context.Context) (*entity.Deployment, error) {
	projectConfig, err := c.GetProjectConfigs(ctx)
	if err != nil {
		return nil, err
	}

	deployment, err := c.gtwy.GetLatestDeploymentForEnvironment(ctx, projectConfig.Project, projectConfig.Environment)
	if err != nil {
		return nil, err
	}

	return deployment, nil
}
