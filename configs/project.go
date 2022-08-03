package configs

import (
	"fmt"
	"os"
	"strings"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (c *Configs) getCWD() (string, error) {
	cwd, err := os.Getwd()
	if err != nil {
		return "", err
	}

	return cwd, nil
}

func (c *Configs) GetProjectConfigs() (*entity.ProjectConfig, error) {
	// Ignore error because the config probably doesn't exist yet
	// TODO: Better error handling here

	userCfg, err := c.GetRootConfigs()
	if err != nil {
		return nil, errors.ProjectConfigNotFound
	}

	// lookup project in global config based on pwd
	cwd, err := c.getCWD()
	if err != nil {
		return nil, err
	}

	// find longest matching parent path
	var longestPath = -1
	var pathMatch = ""
	for path := range userCfg.Projects {
		var matches = strings.HasPrefix(fmt.Sprintf("%s/", cwd), fmt.Sprintf("%s/", path))
		if matches && len(path) > longestPath {
			longestPath = len(path)
			pathMatch = path
		}
	}

	if longestPath == -1 {
		return nil, errors.ProjectConfigNotFound
	}

	projectCfg, found := userCfg.Projects[pathMatch]

	if !found {
		return nil, errors.ProjectConfigNotFound
	}

	return &projectCfg, nil
}

func (c *Configs) SetProjectConfigs(cfg *entity.ProjectConfig) error {
	rootCfg, err := c.GetRootConfigs()
	if err != nil {
		rootCfg = &entity.RootConfig{}
	}

	if rootCfg.Projects == nil {
		rootCfg.Projects = make(map[string]entity.ProjectConfig)
	}

	rootCfg.Projects[cfg.ProjectPath] = *cfg

	return c.SetRootConfig(rootCfg)
}

func (c *Configs) RemoveProjectConfigs(cfg *entity.ProjectConfig) error {
	rootCfg, err := c.GetRootConfigs()
	if err != nil {
		rootCfg = &entity.RootConfig{}
	}

	delete(rootCfg.Projects, cfg.ProjectPath)

	return c.SetRootConfig(rootCfg)
}

func (c *Configs) createNewProjectConfig() (*entity.ProjectConfig, error) {
	cwd, err := c.getCWD()
	if err != nil {
		return nil, err
	}

	projectCfg := &entity.ProjectConfig{
		ProjectPath: cwd,
	}

	return projectCfg, nil
}

func (c *Configs) SetProject(projectID string) error {
	projectCfg, err := c.GetProjectConfigs()

	if err != nil {
		projectCfg, err = c.createNewProjectConfig()

		if err != nil {
			return err
		}
	}

	projectCfg.Project = projectID
	return c.SetProjectConfigs(projectCfg)
}

// SetNewProject configures railway project for current working directory
func (c *Configs) SetNewProject(projectID string) error {
	projectCfg, err := c.createNewProjectConfig()

	if err != nil {
		return err
	}

	projectCfg.Project = projectID
	return c.SetProjectConfigs(projectCfg)
}

func (c *Configs) SetEnvironment(environmentId string) error {
	projectCfg, err := c.GetProjectConfigs()

	if err != nil {
		projectCfg, err = c.createNewProjectConfig()

		if err != nil {
			return err
		}
	}

	projectCfg.Environment = environmentId
	return c.SetProjectConfigs(projectCfg)
}

func (c *Configs) GetProject() (string, error) {
	projectCfg, err := c.GetProjectConfigs()
	if err != nil {
		return "", err
	}

	return projectCfg.Project, nil
}

func (c *Configs) GetCurrentEnvironment() (string, error) {
	projectCfg, err := c.GetProjectConfigs()
	if err != nil {
		return "", err
	}

	return projectCfg.Environment, nil
}
