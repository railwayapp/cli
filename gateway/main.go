package gateway

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	errors2 "github.com/railwayapp/cli/errors"
	"github.com/railwayapp/cli/ui"
	"io"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/pkg/errors"

	configs "github.com/railwayapp/cli/configs"
	"github.com/railwayapp/cli/constants"
)

const (
	CLI_SOURCE_HEADER = "cli"
)

type Gateway struct {
	cfg        *configs.Configs
	httpClient *http.Client
}

func GetHost() string {
	baseURL := "https://backboard.railway.app"
	if configs.IsDevMode() {
		baseURL = "https://backboard.railway-develop.app"
	}
	if configs.IsStagingMode() {
		baseURL = "https://backboard.railway-staging.app"
	}
	return baseURL
}

type AttachCommonHeadersTransport struct{}

func (t *AttachCommonHeadersTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	req.Header.Add("x-source", CLI_SOURCE_HEADER)

	version := constants.Version
	if constants.IsDevVersion() {
		version = "dev"
	}
	req.Header.Set("X-Railway-Version", version)
	return http.DefaultTransport.RoundTrip(req)
}

func New() *Gateway {
	httpClient := &http.Client{
		Timeout:   time.Second * 30,
		Transport: &AttachCommonHeadersTransport{},
	}

	return &Gateway{
		cfg:        configs.New(),
		httpClient: httpClient,
	}
}

type GQLRequest struct {
	q          string
	vars       map[string]interface{}
	header     http.Header
	httpClient *http.Client
}

type GQLError struct {
	Message string `json:"message"`
}

func (e GQLError) Error() string {
	return e.Message
}

type GQLResponse struct {
	Errors []GQLError  `json:"errors"`
	Data   interface{} `json:"data"`
}

func (g *Gateway) authorize(header http.Header) error {
	if g.cfg.RailwayProductionToken != "" {
		header.Add("project-access-token", g.cfg.RailwayProductionToken)
	} else {
		user, err := g.cfg.GetUserConfigs()
		if err != nil {
			return err
		}
		header.Add("authorization", fmt.Sprintf("Bearer %s", user.Token))
	}

	return nil
}

func (g *Gateway) NewRequestWithoutAuth(query string) *GQLRequest {
	gqlReq := &GQLRequest{
		q:          query,
		header:     http.Header{},
		httpClient: g.httpClient,
		vars:       make(map[string]interface{}),
	}

	return gqlReq
}

func (g *Gateway) NewRequestWithAuth(query string) (*GQLRequest, error) {
	gqlReq := g.NewRequestWithoutAuth(query)

	err := g.authorize(gqlReq.header)
	if err != nil {
		return gqlReq, err
	}

	return gqlReq, nil
}

func (r *GQLRequest) Run(ctx context.Context, resp interface{}) error {
	var requestBody bytes.Buffer
	requestBodyObj := struct {
		Query     string                 `json:"query"`
		Variables map[string]interface{} `json:"variables"`
	}{
		Query:     r.q,
		Variables: r.vars,
	}
	if err := json.NewEncoder(&requestBody).Encode(requestBodyObj); err != nil {
		return errors.Wrap(err, "encode body")
	}

	req, err := http.NewRequest(http.MethodPost, fmt.Sprintf("%s/graphql", GetHost()), &requestBody)
	if err != nil {
		return err
	}

	req = req.WithContext(ctx)
	req.Header = r.header
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json; charset=utf-8")
	res, err := r.httpClient.Do(req)
	if err != nil {
		return err
	}
	defer res.Body.Close()

	var buf bytes.Buffer
	if _, err := io.Copy(&buf, res.Body); err != nil {
		return err
	}

	// TODO: Handle auth errors and other things in a special way
	if res.StatusCode < 200 || res.StatusCode >= 300 {
		return fmt.Errorf("Response not successful status=%d", res.StatusCode)
	}

	gr := &GQLResponse{
		Data: resp,
	}
	if err := json.NewDecoder(&buf).Decode(&gr); err != nil {
		return errors.Wrap(err, "decoding response")
	}
	if len(gr.Errors) > 0 {
		messages := make([]string, len(gr.Errors))
		for i, err := range gr.Errors {
			messages[i] = err.Error()
		}

		errText := gr.Errors[0].Message
		if len(gr.Errors) > 1 {
			errText = fmt.Sprintf("%d Errors: %s", len(gr.Errors), strings.Join(messages, ", "))
		}

		// If any GQL responses return fail because unauthenticated, print an error telling the
		// user to log in and exit immediately
		if strings.Contains(errText, "Not Authorized") {
			println(ui.AlertDanger(errors2.UserNotAuthorized.Error()))
			os.Exit(1)
		}

		return errors.New(errText)
	}

	return nil
}

func (r *GQLRequest) Var(name string, value interface{}) {
	r.vars[name] = value
}
