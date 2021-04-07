package controller

import (
	"context"
)

func (c *Controller) GetActiveDeploymentLogs(ctx context.Context) (string, error) {
	projectID, err := c.cfg.GetProject()
	if err != nil {
		return "", err
	}
	environmentID, err := c.cfg.GetEnvironment()
	if err != nil {
		return "", err
	}
	deployments, err := c.gtwy.GetDeploymentsForEnvironment(ctx, projectID, environmentID)
	if err != nil {
		return "", err
	}
	return deployments[0].DeployLogs, nil
}
