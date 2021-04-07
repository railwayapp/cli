package controller

import (
	"context"
	"fmt"
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
	fmt.Println(deployments[0].DeployLogs)
	return "", nil
}
