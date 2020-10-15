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
	client := http.Client{}
	req, err := http.NewRequest(http.MethodGet, fmt.Sprintf("%s/token?token=%s", ENVCACHE_URL, token), nil)
	resp, err := client.Do(req)
	fmt.Println(resp.Body)
	// var resp struct {
	// 	Envs *entity.Envs
	// }
	var envs map[string]string

	err = json.NewDecoder(req.Body).Decode(&envs)
	fmt.Println("DECODED ENVS", envs)
	var respo struct {
		Envs *entity.Envs
	}

	return respo.Envs, nil
}
