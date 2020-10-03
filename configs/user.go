package configs

import (
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (c *Configs) GetUserConfigs() (*entity.UserConfig, error) {
	var cfg entity.UserConfig

	if err := c.unmarshalConfig(c.userConfigs, &cfg); err != nil {
		return nil, errors.UserConfigNotFound
	}
	return &cfg, nil
}

func (c *Configs) SetUserConfigs(cfg *entity.UserConfig) error {
	return c.marshalConfig(c.userConfigs, *cfg)
}
