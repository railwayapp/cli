package controller

import (
	"context"

	"github.com/railwayapp/cli/entity"
	CLIErrors "github.com/railwayapp/cli/errors"
)

// GetCurrentEnvironment returns the currently active environment for the Railway project
func (c *Controller) GetCurrentEnvironment(ctx context.Context) (*entity.Environment, error) {
	projectCfg, err := c.GetProjectConfigs(ctx)
	if err != nil {
		return nil, err
	}

	project, err := c.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return nil, err
	}

	for _, environment := range project.Environments {
		if environment.Id == projectCfg.Environment {
			return environment, nil
		}
	}
	return nil, CLIErrors.EnvironmentNotSet
}

func (c *Controller) GetEnvironmentByName(ctx context.Context, environmentName string) (*entity.Environment, error) {
	projectCfg, err := c.GetProjectConfigs(ctx)
	if err != nil {
		return nil, err
	}

	project, err := c.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return nil, err
	}

	for _, environment := range project.Environments {
		if environment.Name == environmentName {
			return environment, nil
		}
	}
	return nil, CLIErrors.EnvironmentNotFound
}

func (c *Controller) CreateEnvironment(ctx context.Context, req *entity.CreateEnvironmentRequest) (*entity.Environment, error) {
	return c.gtwy.CreateEnvironment(ctx, req)
}

func (c *Controller) CreateEphemeralEnvironment(ctx context.Context, req *entity.CreateEphemeralEnvironmentRequest) (*entity.Environment, error) {
	return c.gtwy.CreateEphemeralEnvironment(ctx, req)
}

func (c *Controller) DeleteEnvironment(ctx context.Context, req *entity.DeleteEnvironmentRequest) error {
	return c.gtwy.DeleteEnvironment(ctx, req)
}
