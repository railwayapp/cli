package controller

import (
	"context"
	b64 "encoding/base64"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"strconv"
	"sync"
	"time"

	"github.com/pkg/browser"
	configs "github.com/railwayapp/cli/configs"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
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

const maxAttempts = 2 * 60
const pollInterval = 1 * time.Second

func (c *Controller) GetUser(ctx context.Context) (*entity.User, error) {
	userCfg, err := c.cfg.GetUserConfigs()
	if err != nil {
		return nil, err
	}
	if userCfg.Token == "" {
		return nil, errors.UserConfigNotFound
	}
	return c.gtwy.GetUser(ctx)
}

func (c *Controller) browserBasedLogin(ctx context.Context) (*entity.User, error) {
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
					_, err = w.Write(byteRes)
					if err != nil {
						fmt.Println("Invalid login response failed to serialize!")
					}
					return
				}

				res := LoginResponse{Status: loginSuccessResponse}
				byteRes, err := json.Marshal(&res)

				if err != nil {
					fmt.Println(err)
				}
				w.WriteHeader(200)
				_, err = w.Write(byteRes)
				if err != nil {
					fmt.Println("Valid login response failed to serialize!")
				}
			} else if r.Method == http.MethodOptions {
				w.Header().Set("Access-Control-Allow-Methods", "GET, HEAD, PUT, PATCH, POST, DELETE")
				w.Header().Set("Access-Control-Allow-Headers", "*")
				w.Header().Set("Content-Length", "0")
				w.WriteHeader(204)
				return
			}

			wg.Done()

			if err := srv.Shutdown(ctx); err != nil {
				fmt.Println(err)
			}
		})

		if err := http.ListenAndServe(fmt.Sprintf("localhost:%d", port), nil); err != nil {
			fmt.Println("Login server handshake failed!")
		}
	}()

	url := getBrowserBasedLoginURL(port, code)
	err = c.ConfirmBrowserOpen("Logging in...", url)

	if err != nil {
		// Opening the browser failed. Try browserless login
		return c.browserlessLogin(ctx)
	}

	fmt.Println("No dice? Try railway login --browserless")

	wg.Wait()

	if code != returnedCode {
		return nil, errors.LoginFailed
	}

	err = c.cfg.SetUserConfigs(&entity.UserConfig{
		Token: token,
	})
	if err != nil {
		return nil, err
	}

	user, err := c.gtwy.GetUser(ctx)
	if err != nil {
		return nil, err
	}

	return user, nil
}

func (c *Controller) pollForToken(ctx context.Context, code string) (string, error) {
	var count = 0
	for count < maxAttempts {
		token, err := c.gtwy.ConsumeLoginSession(ctx, code)

		if err != nil {
			return "", errors.LoginFailed
		}

		if token != "" {
			return token, nil
		}

		count++
		time.Sleep(pollInterval)
	}

	return "", errors.LoginTimeout
}

func (c *Controller) browserlessLogin(ctx context.Context) (*entity.User, error) {
	wordCode, err := c.gtwy.CreateLoginSession(ctx)
	if err != nil {
		return nil, err
	}

	url := getBrowserlessLoginURL(wordCode)

	fmt.Printf("Your pairing code is: %s\n", ui.MagentaText(wordCode))
	fmt.Printf("To authenticate with Railway, please go to \n    %s\n", url)

	token, err := c.pollForToken(ctx, wordCode)
	if err != nil {
		return nil, err
	}

	err = c.cfg.SetUserConfigs(&entity.UserConfig{
		Token: token,
	})
	if err != nil {
		return nil, err
	}

	user, err := c.gtwy.GetUser(ctx)
	if err != nil {
		return nil, err
	}

	return user, nil
}

func (c *Controller) Login(ctx context.Context, isBrowserless bool) (*entity.User, error) {
	// Invalidate current session if it exists
	if loggedIn, _ := c.IsLoggedIn(ctx); loggedIn {
		if err := c.gtwy.Logout(ctx); err != nil {
			return nil, err
		}
	}

	if isBrowserless || isSSH() || isCodeSpaces() {
		return c.browserlessLogin(ctx)
	}

	return c.browserBasedLogin(ctx)
}

func (c *Controller) Logout(ctx context.Context) error {
	if loggedIn, _ := c.IsLoggedIn(ctx); !loggedIn {
		fmt.Printf("🚪  %s\n", ui.YellowText("Already logged out"))
		return nil
	}

	err := c.gtwy.Logout(ctx)
	if err != nil {
		return err
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

func (c *Controller) ConfirmBrowserOpen(spinnerMsg string, url string) error {
	fmt.Printf("Press Enter to open the browser (^C to quit)")
	fmt.Fscanln(os.Stdin)
	ui.StartSpinner(&ui.SpinnerCfg{
		Message: spinnerMsg,
	})

	err := browser.OpenURL(url)

	if err != nil {
		ui.StopSpinner("Failed to open browser, attempting browserless login.")
		return err
	}

	return nil
}

func getAPIURL() string {
	if configs.IsDevMode() {
		return baseLocalhostURL
	}
	return baseRailwayURL
}

func getHostName() string {
	name, err := os.Hostname()
	if err != nil {
		return ""
	}

	return name
}

func getBrowserBasedLoginURL(port int, code string) string {
	hostname := getHostName()
	buffer := b64.URLEncoding.EncodeToString([]byte(fmt.Sprintf("port=%d&code=%s&hostname=%s", port, code, hostname)))
	url := fmt.Sprintf("%s/cli-login?d=%s", getAPIURL(), buffer)
	return url
}

func getBrowserlessLoginURL(wordCode string) string {
	hostname := getHostName()
	buffer := b64.URLEncoding.EncodeToString([]byte(fmt.Sprintf("wordCode=%s&hostname=%s", wordCode, hostname)))

	url := fmt.Sprintf("%s/cli-login?d=%s", getAPIURL(), buffer)
	return url
}

func isSSH() bool {
	if os.Getenv("SSH_TTY") != "" || os.Getenv("SSH_CONNECTION") != "" || os.Getenv("SSH_CLIENT") != "" {
		return true
	}

	return false
}

func isCodeSpaces() bool {
	return os.Getenv("CODESPACES") == "true"
}
