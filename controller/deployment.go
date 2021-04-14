package controller

import (
	"context"
	"github.com/railwayapp/cli/entity"
)

func (c *Controller) GetDeployments(ctx context.Context) ([]*entity.Deployment, error) {
	projectID, err := c.cfg.GetProject()
	if err != nil {
		return nil, err
	}
	environmentID, err := c.cfg.GetEnvironment()
	if err != nil {
		return nil, err
	}
	return c.gtwy.GetDeploymentsForEnvironment(ctx, projectID, environmentID)
}

func (c *Controller) GetActiveDeployment(ctx context.Context) (*entity.Deployment, error) {
	projectID, err := c.cfg.GetProject()
	if err != nil {
		return nil, err
	}

	environmentID, err := c.cfg.GetEnvironment()
	if err != nil {
		return nil, err
	}

	deployment, err := c.gtwy.GetLatestDeploymentForEnvironment(ctx, projectID, environmentID)
	if err != nil {
		return nil, err
	}

	return deployment, nil
}
