package controller

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

// GetCurrentProject returns the currently active project
func (c *Controller) GetCurrentProject(ctx context.Context) (*entity.Project, error) {
	projectCfg, err := c.GetProjectConfigs(ctx)
	if err != nil {
		return nil, err
	}

	project, err := c.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return nil, err
	}

	return project, nil
}

// GetProject returns a project of id projectId, error otherwise
func (c *Controller) GetProject(ctx context.Context, projectId string) (*entity.Project, error) {
	return c.gtwy.GetProject(ctx, projectId)
}

// GetProjectByName returns a project for the user of name projectName, error otherwise
func (c *Controller) GetProjectByName(ctx context.Context, projectName string) (*entity.Project, error) {
	return c.gtwy.GetProjectByName(ctx, projectName)
}

// CreateProject creates a project specified by the project request, error otherwise
func (c *Controller) CreateProject(ctx context.Context, req *entity.CreateProjectRequest) (*entity.Project, error) {
	return c.gtwy.CreateProject(ctx, req)
}

// CreateProjectFromTemplate creates a project from template specified by the project request, error otherwise
func (c *Controller) CreateProjectFromTemplate(ctx context.Context, req *entity.CreateProjectFromTemplateRequest) (*entity.CreateProjectFromTemplateResult, error) {
	return c.gtwy.CreateProjectFromTemplate(ctx, req)
}

// UpdateProject updates a project specified by the project request, error otherwise
func (c *Controller) UpdateProject(ctx context.Context, req *entity.UpdateProjectRequest) (*entity.Project, error) {
	return c.gtwy.UpdateProject(ctx, req)
}

// GetProjects returns all projects associated with the user, error otherwise
func (c *Controller) GetProjects(ctx context.Context) ([]*entity.Project, error) {
	return c.gtwy.GetProjects(ctx)
}

// OpenProjectInBrowser opens the provided projectId in the browser
func (c *Controller) OpenProjectInBrowser(ctx context.Context, projectID string, environmentID string) error {
	return c.gtwy.OpenProjectInBrowser(projectID, environmentID)
}

// OpenProjectPathInBrowser opens the provided projectId with the provided path in the browser
func (c *Controller) OpenProjectPathInBrowser(ctx context.Context, projectID string, environmentID string, path string) error {
	return c.gtwy.OpenProjectPathInBrowser(projectID, environmentID, path)
}

// OpenProjectDeploymentsInBrowser opens the provided projectId's depolyments in the browser
func (c *Controller) OpenProjectDeploymentsInBrowser(ctx context.Context, projectID string) error {
	return c.gtwy.OpenProjectDeploymentsInBrowser(projectID)
}

// GetProjectDeploymentsURL returns the URL to access project deployment in browser
func (c *Controller) GetProjectDeploymentsURL(ctx context.Context, projectID string) string {
	return c.gtwy.GetProjectDeploymentsURL(projectID)
}

// GetServiceDeploymentsURL returns the URL to access service deployments in the browser
func (c *Controller) GetServiceDeploymentsURL(ctx context.Context, projectID string, serviceID string, deploymentID string) string {
	return c.gtwy.GetServiceDeploymentsURL(projectID, serviceID, deploymentID)
}

// GetLatestDeploymentForEnvironment returns the URL to access project deployment in browser
func (c *Controller) GetLatestDeploymentForEnvironment(ctx context.Context, projectID string, environmentID string) (*entity.Deployment, error) {
	return c.gtwy.GetLatestDeploymentForEnvironment(ctx, projectID, environmentID)
}

func (c *Controller) OpenStaticUrlInBrowser(staticUrl string) error {
	return c.gtwy.OpenStaticUrlInBrowser(staticUrl)
}

func (c *Controller) DeleteProject(ctx context.Context, projectID string) error {
	return c.gtwy.DeleteProject(ctx, projectID)
}
