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
	deploy, err := c.gtwy.GetDeploymentByID(ctx, req.ProjectID, req.DeploymentID)
	if err != nil {
		return err
	}
	logLines := strings.Split(deploy.DeployLogs, "\n")
	lineNums := int(req.NumLines)
	if lineNums == 0 {
		lineNums = len(logLines)
	}
	offset := math.Max(float64(len(logLines))-float64(lineNums)-1, 0.0)
	fmt.Print(strings.Join(logLines[int(offset):], "\n"))
	if req.NumLines == 0 {
		prevLogs := strings.Split(deploy.DeployLogs, "\n")
		for {
			time.Sleep(time.Second * 2)
			deploy, err := c.gtwy.GetDeploymentByID(ctx, req.ProjectID, req.DeploymentID)
			if err != nil {
				return err
			}
			currLogs := strings.Split(deploy.DeployLogs, "\n")
			out := LogDiff(LogDiffReq{
				Prev: prevLogs,
				Next: currLogs,
			})
			if len(out) == 0 {
				continue
			}
			fmt.Print(strings.Join(out, "\n"))
			prevLogs = currLogs
		}
	}
	return nil
}

type LogDiffReq struct {
	Prev  []string
	Next  []string
	Limit int32
}

func LogDiff(req LogDiffReq) []string {
	return req.Next[len(req.Prev)-1 : len(req.Next)-1]
}
