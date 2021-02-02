package configs

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

// MigrateLocalProjectConfig moves a local project config
// to the global config and removes the local .railway directory
// if it exists
func (c *Configs) MigrateLocalProjectConfig() error {
	// Get local config directory
	projectDir, err := filepath.Abs(filepath.Dir(filepath.Dir(c.projectConfigs.configPath)))
	if err != nil {
		return err
	}

	// Avoid deleting ~/.railway
	if projectDir == os.Getenv("HOME") {
		return nil
	}

	if _, err := os.Stat(projectDir); os.IsNotExist(err) {
		// Local project directory does not exist
		return nil
	}

	// Read local project config
	var cfg entity.ProjectConfig
	if err := c.unmarshalConfig(c.projectConfigs, &cfg); err != nil {
		return err
	}

	// Save project config to root config
	cfg.ProjectPath = strings.ToLower(projectDir)
	if err = c.SetProjectConfigs(&cfg); err != nil {
		return err
	}

	// Delete local config directory
	if err = os.RemoveAll(fmt.Sprintf("%s/.railway", projectDir)); err != nil {
		return err
	}

	return nil
}

func (c *Configs) getCWD() (string, error) {
	cwd, err := os.Getwd()
	if err != nil {
		return "", err
	}
	cwd = strings.ToLower(cwd)

	return cwd, nil
}

func (c *Configs) GetProjectConfigs() (*entity.ProjectConfig, error) {
	c.MigrateLocalProjectConfig()

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

func (c *Configs) GetEnvironment() (string, error) {
	projectCfg, err := c.GetProjectConfigs()
	if err != nil {
		return "", err
	}

	return projectCfg.Environment, nil
}
