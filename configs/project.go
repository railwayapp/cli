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

func (c *Configs) getPWD() (string, error) {
	pwd, err := os.Getwd()
	if err != nil {
		return "", err
	}
	pwd = strings.ToLower(pwd)

	return pwd, nil
}

func (c *Configs) GetProjectConfigs() (*entity.ProjectConfig, error) {
	c.MigrateLocalProjectConfig()

	userCfg, err := c.GetRootConfigs()
	if err != nil {
		return nil, errors.ProjectConfigNotFound
	}

	// lookup project in global config based on pwd
	pwd, err := c.getPWD()
	if err != nil {
		return nil, err
	}

	projectCfg, found := userCfg.Projects[pwd]

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

func (c *Configs) createNewProjectConfig() (*entity.ProjectConfig, error) {
	pwd, err := c.getPWD()
	if err != nil {
		return nil, err
	}

	projectCfg := &entity.ProjectConfig{
		ProjectPath: pwd,
	}

	return projectCfg, nil
}

func (c *Configs) SetProject(projectId string) error {
	projectCfg, err := c.GetProjectConfigs()

	if err != nil {
		projectCfg, err = c.createNewProjectConfig()

		if err != nil {
			return err
		}
	}

	projectCfg.Project = projectId
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
