package controller

import (
	"context"
	b64 "encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"sync"

	"github.com/pkg/browser"
	configs "github.com/railwayapp/cli/configs"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

const (
	baseRailwayURL   string = "https://railway.app"
	baseLocalhostURL string = "http://localhost:3000"
)

const (
	loginInvalidResponse string = "Invalid code"
	loginSuccessResponse string = "Ok"
)

type LoginResponse struct {
	Status string `json:"status,omitempty"`
	Error  string `json:"error,omitempty"`
}

func (c *Controller) GetUser(ctx context.Context) (*entity.User, error) {
	userCfg, err := c.cfg.GetUserConfigs()
	if err != nil {
		return nil, err
	}
	if userCfg.Token == "" {
		return nil, errors.New("Not logged in")
	}
	return c.gtwy.GetUser(ctx)
}

func (c *Controller) Login(ctx context.Context) (*entity.User, error) {
	var token string
	var returnedCode string
	port, err := c.randomizer.Port()
	if err != nil {
		return nil, err
	}
	code := c.randomizer.Code()
	wg := &sync.WaitGroup{}
	wg.Add(1)
	go func() {
		ctx := context.Background()
		srv := &http.Server{Addr: strconv.Itoa(port)}
		http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Access-Control-Allow-Origin", getAPIURL())
			if r.Method == http.MethodGet {
				w.Header().Set("Content-Type", "application/json")
				token = r.URL.Query().Get("token")
				returnedCode = r.URL.Query().Get("code")

				if code != returnedCode {
					res := LoginResponse{Error: loginInvalidResponse}
					byteRes, err := json.Marshal(&res)
					if err != nil {
						fmt.Println(err)
					}
					w.WriteHeader(400)
					w.Write(byteRes)
					return
				}
				res := LoginResponse{Status: loginSuccessResponse}
				byteRes, err := json.Marshal(&res)
				if err != nil {
					fmt.Println(err)
				}
				w.WriteHeader(200)
				w.Write(byteRes)
			} else if r.Method == http.MethodOptions {
				w.Header().Set("Access-Control-Allow-Methods", "GET, HEAD, PUT, PATCH, POST, DELETE")
				w.Header().Set("Access-Control-Allow-Headers", "authorization")
				w.Header().Set("Content-Length", "0")
				w.WriteHeader(204)
				return
			}
			wg.Done()
			if err := srv.Shutdown(ctx); err != nil {
				fmt.Println(err)
			}
		})
		http.ListenAndServe(fmt.Sprintf("localhost:%d", port), nil)
	}()
	url := getLoginURL(port, code)
	browser.OpenURL(url)
	wg.Wait()
	err = c.cfg.SetUserConfigs(&entity.UserConfig{
		Token: token,
	})
	if err != nil {
		return nil, err
	}
	if code == returnedCode {
		return c.gtwy.GetUser(ctx)
	}
	return nil, nil
}

func (c *Controller) Logout(ctx context.Context) error {
	// Logout by wiping user configs
	userCfg, err := c.cfg.GetUserConfigs()
	if err != nil {
		return err
	}
	if userCfg.Token == "" {
		fmt.Printf("🚪  %s\n", ui.YellowText("Already logged out"))
		return nil
	}
	err = c.cfg.SetUserConfigs(&entity.UserConfig{})
	if err != nil {
		return err
	}
	fmt.Printf("👋 %s\n", ui.YellowText("Logged out"))
	return nil
}

func (c *Controller) IsLoggedIn(ctx context.Context) (bool, error) {
	userCfg, err := c.cfg.GetUserConfigs()
	if err != nil {
		return false, err
	}
	isLoggedIn := userCfg.Token != ""
	return isLoggedIn, nil
}

func getAPIURL() string {
	if configs.IsDevMode() {
		return baseLocalhostURL
	}
	return baseRailwayURL
}

func getLoginURL(port int, code string) string {
	buffer := b64.URLEncoding.EncodeToString([]byte(fmt.Sprintf("port=%d&code=%s", port, code)))
	url := fmt.Sprintf("%s/cli-login?d=%s", getAPIURL(), buffer)
	return url
}
