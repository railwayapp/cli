package controller

import (
	"context"
	"fmt"
	"strings"
	"time"
)

func (c *Controller) GetActiveDeploymentLogs(ctx context.Context, detached bool) error {
	projectID, err := c.cfg.GetProject()
	if err != nil {
		return err
	}
	environmentID, err := c.cfg.GetEnvironment()
	if err != nil {
		return err
	}
	fmt.Printf("Fetching latest deployment...")
	deployments, err := c.gtwy.GetDeploymentsForEnvironment(ctx, projectID, environmentID)
	if err != nil {
		return err
	}
	fmt.Println("✅")

	latestDeploy := deployments[0]
	// Streaming
	prevIdx := 0
	for {
		err := func() error {
			if prevIdx != 0 {
				time.Sleep(time.Second * 2)
			}
			deploy, err := c.gtwy.GetDeploymentByID(ctx, projectID, latestDeploy.ID)
			if err != nil {
				return err
			}
			partials := strings.Split(deploy.DeployLogs, "\n")
			nextIdx := len(partials)
			delta := partials[prevIdx:nextIdx]
			if len(delta) == 0 {
				return nil
			}
			fmt.Println(strings.Join(delta, "\n"))
			prevIdx = nextIdx
			return nil
		}()
		if err != nil {
			return err
		}
		if detached {
			return nil
		}
	}
}
