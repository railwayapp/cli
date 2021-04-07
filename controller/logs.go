package controller

import (
	"context"
	"fmt"
	"math"
	"strings"
	"time"

	"github.com/railwayapp/cli/entity"
)

func (c *Controller) GetActiveDeploymentLogs(ctx context.Context, numLines int32) error {
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
	status := latestDeploy.Status
	for {
		err := func() error {
			if prevIdx != 0 {
				time.Sleep(time.Second * 2)
			}
			deploy, err := c.gtwy.GetDeploymentByID(ctx, projectID, latestDeploy.ID)
			if err != nil {
				return err
			}
			if deploy.Status != status {
				// Reset when moving states
				prevIdx = 0
				status = deploy.Status
			}
			logs := fetchCurrentLogs(*deploy)
			partials := strings.Split(logs, "\n")
			nextIdx := len(partials)
			delimiter := prevIdx
			if numLines != 0 {
				// If num is provided do a walkback by n lines to get latest n logs
				delimiter = int(math.Max(float64(len(partials)-int(numLines)), float64(prevIdx)))
			}
			delta := partials[delimiter:nextIdx]
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
		if numLines != 0 {
			// Break if numlines provided
			return nil
		}
	}
}

func fetchCurrentLogs(deployment entity.Deployment) string {
	if deployment.Status == "BUILDING" {
		return deployment.BuildLogs
	}
	return deployment.DeployLogs
}
