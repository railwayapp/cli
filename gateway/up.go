package gateway

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io/ioutil"
	"mime/multipart"
	"net/http"

	"github.com/railwayapp/cli/entity"
)

func constructReq(ctx context.Context, req *entity.UpRequest) (*http.Request, error) {
	body := new(bytes.Buffer)
	writer := multipart.NewWriter(body)
	part, err := writer.CreateFormFile("file", req.ProjectID)
	if err != nil {
		return nil, err
	}
	part.Write(req.Data.Bytes())

	err = writer.Close()
	if err != nil {
		return nil, err
	}
	url := fmt.Sprintf("%s/project/%s/up", GetHost(), req.ProjectID)
	httpReq, err := http.NewRequest("POST", url, body)
	if err != nil {
		return nil, err
	}
	httpReq.Header.Set("Content-Type", writer.FormDataContentType())
	return httpReq, nil
}

func (g *Gateway) Up(ctx context.Context, req *entity.UpRequest) (*entity.UpResponse, error) {
	httpReq, err := constructReq(ctx, req)
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
		return nil, errors.New(string(bodyBytes))
	}
	var res entity.UpResponse
	if err := json.Unmarshal(bodyBytes, &res); err != nil {
		return nil, err
	}
	return &res, nil
}
