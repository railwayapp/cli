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
	deployment, err := c.gtwy.GetLatestDeploymentForEnvironment(ctx, projectID, environmentID)
	if err != nil {
		return err
	}

	return c.LogsForDeployment(ctx, &entity.DeploymentLogsRequest{
		DeploymentID: deployment.ID,
		ProjectID:    projectID,
		NumLines:     numLines,
	})
}

func (c *Controller) LogsForDeployment(ctx context.Context, req *entity.DeploymentLogsRequest) error {
	// LogsForDeployment will do one of two things:
	// 1) If numLines is provided, perform a single request and get the last n lines
	// 2) If numLines is not provided, poll for deploymentLogs while keeping a pointer for the line number
	//    This pointer will be used to determine what to send to stdout
	//    e.g We fetch 10 lines initially. Subsequent fetch returns 12. We print the last 2 lines (delta)
	prevIdx := 1
	for {
		if prevIdx != 1 {
			time.Sleep(time.Second * 2)
		}
		deploy, err := c.gtwy.GetDeploymentByID(ctx, req.ProjectID, req.DeploymentID)
		if err != nil {
			return err
		}
		partials := strings.Split(deploy.DeployLogs, "\n")
		nextIdx := len(partials)
		delimiter := prevIdx
		if req.NumLines != 0 {
			// If num is provided do a walkback by n lines to get latest n logs
			delimiter = int(math.Max(float64(len(partials)-int(req.NumLines)), float64(prevIdx)))
		}
		delta := partials[delimiter-1 : nextIdx]
		if len(delta) == 0 {
			continue
		}
		fmt.Printf(strings.Join(delta, "\n"))
		prevIdx = nextIdx
		if req.NumLines != 0 {
			// Break if numlines provided
			return nil
		}
	}
}
