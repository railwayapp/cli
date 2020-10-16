package controller

import (
	"github.com/google/go-github/github"
	"github.com/railwayapp/cli/configs"
	"github.com/railwayapp/cli/gateway"
	"github.com/railwayapp/cli/random"
)

type Controller struct {
	gtwy       *gateway.Gateway
	cfg        *configs.Configs
	randomizer *random.Randomizer
	ghc        *github.Client
}

func New() *Controller {
	return &Controller{
		gtwy:       gateway.New(),
		cfg:        configs.New(),
		randomizer: random.New(),
		ghc:        github.NewClient(nil),
	}
}
