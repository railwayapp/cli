package controller

import (
	"github.com/railwayapp/cli/configs"
	"github.com/railwayapp/cli/gateway"
	"github.com/railwayapp/cli/random"
)

type Controller struct {
	gtwy       *gateway.Gateway
	cfg        *configs.Configs
	randomizer *random.Randomizer
}

func New() *Controller {
	return &Controller{
		gtwy:       gateway.New(),
		cfg:        configs.New(),
		randomizer: random.New(),
	}
}
