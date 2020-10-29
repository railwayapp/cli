package configs

import (
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (c *Configs) GetProjectConfigs() (*entity.ProjectConfig, error) {
	var cfg entity.ProjectConfig

	if err := c.unmarshalConfig(c.projectConfigs, &cfg); err != nil {
		return nil, errors.ProjectConfigNotFound
	}
	return &cfg, nil
}

func (c *Configs) SetProjectConfigs(cfg *entity.ProjectConfig) error {
	return c.marshalConfig(c.projectConfigs, *cfg)
}

func (c *Configs) SaveProjectConfig() error {
	err := c.CreatePathIfNotExist(c.rootConfig.configPath)
	if err != nil {
		return err
	}

	err = c.projectConfigs.viper.WriteConfig()
	return err
}

func (c *Configs) SetProject(projectId string) error {
	c.projectConfigs.viper.Set("project", projectId)
	return c.SaveProjectConfig()
}

func (c *Configs) SetEnvironment(environmentId string) error {
	c.projectConfigs.viper.Set("environment", environmentId)
	return c.SaveProjectConfig()
}

func (c *Configs) GetProject() (string, error) {
	err := c.projectConfigs.viper.ReadInConfig()
	if err != nil {
		return "", errors.ProjectConfigNotFound
	}
	return c.projectConfigs.viper.GetString("project"), nil
}

func (c *Configs) GetEnvironment() (string, error) {
	err := c.projectConfigs.viper.ReadInConfig()
	if err != nil {
		return "", err
	}
	return c.projectConfigs.viper.GetString("environment"), nil
}
