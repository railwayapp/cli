package controller

import (
	"context"

	"github.com/railwayapp/cli/entity"
)

// GetProject returns a project of id projectId, error otherwise
func (c *Controller) GetProject(ctx context.Context, projectId string) (*entity.Project, error) {
	return c.gtwy.GetProject(ctx, projectId)
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
func (c *Controller) OpenProjectInBrowser(ctx context.Context, projectID string, environmentID string) {
	c.gtwy.OpenProjectInBrowser(projectID, environmentID)
}

// OpenProjectDeploymentsInBrowser opens the provided projectId's depolyments in the browser
func (c *Controller) OpenProjectDeploymentsInBrowser(ctx context.Context, projectID string) {
	c.gtwy.OpenProjectDeploymentsInBrowser(projectID)
}
