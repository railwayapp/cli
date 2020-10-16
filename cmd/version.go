package cmd

import (
	"context"
	"fmt"

	"github.com/google/go-github/github"
	"github.com/railwayapp/cli/constants"
	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Version(ctx context.Context, req *entity.CommandRequest) error {
	fmt.Println(fmt.Sprintf("railway version %s", constants.Version))
	if constants.Version != "source" {
		latest, err := getLatestVersion()
		if err != nil {
			return err
		}
		if latest != "" && latest != constants.Version {
			fmt.Println("A newer version of the Railway CLI is available, please update to:", latest)
		}
	}
	return nil
}

func getLatestVersion() (string, error) {
	client := github.NewClient(nil)
	rep, _, err := client.Repositories.GetLatestRelease(context.Background(), "railwayapp", "cli")
	return *rep.TagName, err
}
