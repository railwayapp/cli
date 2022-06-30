package gateway

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io/ioutil"
	"net/http"

	"github.com/railwayapp/cli/entity"
)

func constructReq(ctx context.Context, req *entity.UpRequest) (*http.Request, error) {
	url := fmt.Sprintf("%s/project/%s/environment/%s/up?serviceId=%s", GetHost(), req.ProjectID, req.EnvironmentID, req.ServiceID)
	httpReq, err := http.NewRequestWithContext(ctx, "POST", url, &req.Data)
	if err != nil {
		return nil, err
	}
	httpReq.Header.Set("Content-Type", "multipart/form-data")
	return httpReq, nil
}

func (g *Gateway) Up(ctx context.Context, req *entity.UpRequest) (*entity.UpResponse, error) {
	httpReq, err := constructReq(ctx, req)
	if err != nil {
		return nil, err
	}
	err = g.authorize(httpReq.Header)
	if err != nil {
		return nil, err
	}
	client := &http.Client{}
	resp, err := client.Do(httpReq)
	if err != nil {
		return nil, err
	}

	bodyBytes, err := ioutil.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}

	if resp.StatusCode < 200 || resp.StatusCode >= 400 {
		var res entity.UpErrorResponse
		// Try decoding up's error response and fallback to sending body as text if decoding fails
		if err := json.Unmarshal(bodyBytes, &res); err != nil {
			return nil, errors.New(string(bodyBytes))
		} else {
			return nil, errors.New(res.Message)
		}
	}

	var res entity.UpResponse
	if err := json.Unmarshal(bodyBytes, &res); err != nil {
		return nil, err
	}
	return &res, nil
}
