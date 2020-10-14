package gateway

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"

	"github.com/railwayapp/cli/entity"

	"github.com/railwayapp/cli/common/configs"
)

var ENVCACHE_URL = configs.ENVCACHE_URL

func (g *Gateway) GetEnvcacheWithProjectToken(ctx context.Context) (*entity.Envs, error) {
	token, err := g.getProjectToken(ctx)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequest(http.MethodGet, fmt.Sprintf("%s/token?token=%s", ENVCACHE_URL, token), nil)

	var resp struct {
		Envs *entity.Envs
	}

	err = json.NewDecoder(req.Body).Decode(&resp)

	return resp.Envs, nil
}
