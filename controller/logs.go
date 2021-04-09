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
	// Fetch Initial Deployment Logs
	query := entity.DeploymentGQL{
		DeployLogs: true,
	}
	deploy, err := c.gtwy.GetDeploymentByID(ctx, &entity.DeploymentByIDRequest{
		DeploymentID: req.DeploymentID,
		ProjectID:    req.ProjectID,
		GQL:          query,
	})
	if err != nil {
		return err
	}
	// Break them down line by line
	logLines := strings.Split(fetchCurrentLogs(deploy), "\n")
	offset := 0.0
	if req.NumLines != 0 {
		// If a limit is set, walk it back n steps (with a min of zero so no panics)
		offset = math.Max(float64(len(logLines))-float64(req.NumLines)-1, 0.0)
	}
	// Output Initial Logs
	fmt.Println(strings.Join(logLines[int(offset):], "\n"))
	if req.NumLines == 0 {
		// If no log limit is set, we stream logs
		idxMp := make(map[string][]string)
		idxMp[deploy.Status] = strings.Split(deploy.DeployLogs, "\n")
		for {
			time.Sleep(time.Second * 2)
			deploy, err := c.gtwy.GetDeploymentByID(ctx, &entity.DeploymentByIDRequest{
				DeploymentID: req.DeploymentID,
				ProjectID:    req.ProjectID,
				GQL:          query,
			})
			if err != nil {
				return err
			}
			// Current Logs fetched from server
			currLogs := strings.Split(fetchCurrentLogs(deploy), "\n")
			// Diff logs using the line numbers as references
			idx := int(math.Max(float64(len(idxMp[deploy.Status])-1), 0.0))
			logDiff := currLogs[idx : len(currLogs)-1]
			// If no changes we continue
			if len(logDiff) == 0 {
				continue
			}
			// Output logs
			fmt.Println(strings.Join(logDiff, "\n"))
			// Set out walk pointer forward using the newest logs
			idxMp[deploy.Status] = currLogs
		}
	}
	return nil
}

func fetchCurrentLogs(deployment *entity.Deployment) string {
	if deployment.Status == "BUILDING" {
		return deployment.BuildLogs
	}
	return deployment.DeployLogs
}
