package configs

import (
	"github.com/railwayapp/cli/entity"
)

func (c *Configs) GetRootConfigs() (*entity.RootConfig, error) {
	var cfg entity.RootConfig

	if err := c.unmarshalConfig(c.rootConfigs, &cfg); err != nil {
		return nil, err
	}
	return &cfg, nil
}

func (c *Configs) SetRootConfig(cfg *entity.RootConfig) error {
	if cfg.Projects == nil {
		cfg.Projects = make(map[string]entity.ProjectConfig)
	}

	return c.marshalConfig(c.rootConfigs, *cfg)
}
