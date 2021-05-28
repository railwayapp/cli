package controller

import (
	"context"
	"errors"
	"fmt"
	"math"
	"strings"
	"time"

	"github.com/railwayapp/cli/entity"
)

const (
	GQL_SOFT_ERROR = "Error fetching build logs"
)

func (c *Controller) GetActiveDeploymentLogs(ctx context.Context, numLines int32) error {
	deployment, err := c.GetActiveDeployment(ctx)
	if err != nil {
		return err
	}

	return c.logsForState(ctx, &entity.DeploymentLogsRequest{
		DeploymentID: deployment.ID,
		ProjectID:    deployment.ProjectID,
		NumLines:     numLines,
	})
}

func (c *Controller) GetActiveBuildLogs(ctx context.Context, numLines int32) error {
	projectConfig, err := c.GetProjectConfigs(ctx)
	if err != nil {
		return err
	}

	deployment, err := c.gtwy.GetLatestDeploymentForEnvironment(ctx, projectConfig.Project, projectConfig.Environment)
	if err != nil {
		return err
	}

	return c.logsForState(ctx, &entity.DeploymentLogsRequest{
		DeploymentID: deployment.ID,
		ProjectID:    projectConfig.Project,
		NumLines:     numLines,
	})
}

/* Logs for state will get logs for a current state (Either building or not building state)
   It does this by capturing the initial state of the deploy, and looping while it stays in that state
   The loop captures the previous deploy as well as the current and does log diffing on the unified state
   When the state transitions from building to not building, the loop breaks
*/
func (c *Controller) logsForState(ctx context.Context, req *entity.DeploymentLogsRequest) error {
	// Stream on building -> Building until !Building then break
	// Stream on not building -> !Building until Failed then break
	deploy, err := c.gtwy.GetDeploymentByID(ctx, &entity.DeploymentByIDRequest{
		DeploymentID: req.DeploymentID,
		ProjectID:    req.ProjectID,
		GQL:          c.getQuery(ctx, ""),
	})
	if err != nil {
		return err
	}

	// Print Logs w/ Limit
	logLines := strings.Split(logsForState(ctx, deploy.Status, deploy), "\n")
	offset := 0.0
	if req.NumLines != 0 {
		// If a limit is set, walk it back n steps (with a min of zero so no panics)
		offset = math.Max(float64(len(logLines))-float64(req.NumLines)-1, 0.0)
	}
	// GQL may return partial errors for build logs if not ready
	// The response won't fail but will be a partial error. Check this.
	err = errFromGQL(ctx, logLines)
	if err != nil {
		return err
	}

	// Output Initial Logs
	currLogs := strings.Join(logLines[int(offset):], "\n")
	if len(currLogs) > 0 {
		fmt.Println(currLogs)
	}

	if deploy.Status == entity.STATUS_FAILED {
		return errors.New("Build Failed! Please see output for more information")
	}

	prevDeploy := deploy
	logState := deploy.Status
	deltaState := hasTransitioned(nil, deploy)

	for !deltaState && req.NumLines == 0 {
		time.Sleep(2 * time.Second)
		currDeploy, err := c.gtwy.GetDeploymentByID(ctx, &entity.DeploymentByIDRequest{
			DeploymentID: req.DeploymentID,
			ProjectID:    req.ProjectID,
			GQL:          c.getQuery(ctx, logState),
		})
		if err != nil {
			return err
		}
		// Current Logs fetched from server
		currLogs := strings.Split(logsForState(ctx, logState, currDeploy), "\n")
		// Previous logs fetched from prevDeploy variable
		prevLogs := strings.Split(logsForState(ctx, logState, prevDeploy), "\n")
		// Diff logs using the line numbers as references
		logDiff := currLogs[len(prevLogs)-1 : len(currLogs)-1]
		// If no changes we continue
		if len(logDiff) == 0 {
			continue
		}
		// Output logs
		fmt.Println(strings.Join(logDiff, "\n"))
		// Set out walk pointer forward using the newest logs
		deltaState = hasTransitioned(prevDeploy, currDeploy)
		prevDeploy = currDeploy
	}
	return nil
}

func hasTransitioned(prev *entity.Deployment, curr *entity.Deployment) bool {
	return prev != nil && curr != nil && prev.Status != curr.Status
}

func (c *Controller) getQuery(ctx context.Context, status string) entity.DeploymentGQL {
	return entity.DeploymentGQL{
		BuildLogs:  status == entity.STATUS_BUILDING || status == "",
		DeployLogs: status != entity.STATUS_BUILDING || status == "",
		Status:     true,
	}
}

func logsForState(ctx context.Context, status string, deploy *entity.Deployment) string {
	if status == entity.STATUS_BUILDING {
		return deploy.BuildLogs
	}
	return deploy.DeployLogs
}

func errFromGQL(ctx context.Context, logLines []string) error {
	for _, l := range logLines {
		if strings.Contains(l, GQL_SOFT_ERROR) {
			return errors.New(GQL_SOFT_ERROR)
		}
	}
	return nil
}
