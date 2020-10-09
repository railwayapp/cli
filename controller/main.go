package controller

import (
	"github.com/railwayapp/cli/configs"
	githubGateway "github.com/railwayapp/cli/gateway/github"
	railwayGateway "github.com/railwayapp/cli/gateway/railway"

	"github.com/railwayapp/cli/random"
)

type Controller struct {
	rwGateway  *railwayGateway.Gateway
	ghGateway  *githubGateway.Gateway
	cfg        *configs.Configs
	randomizer *random.Randomizer
}

func New() *Controller {
	return &Controller{
		rwGateway:  railwayGateway.New(),
		ghGateway:  githubGateway.New(),
		cfg:        configs.New(),
		randomizer: random.New(),
	}
}
