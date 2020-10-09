package cmd

import (
	"context"
	"fmt"
	"strings"

	"os/exec"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Template(ctx context.Context, req *entity.CommandRequest) error {
	dirs := []string{}
	prompts := []string{"Select Language", "Select Type", "Select Template"}
	for i := 0; i < 2; i++ {
		options, err := h.ctrl.GetRailwayTemplates(ctx, strings.Join(dirs[:], "/"))
		if err != nil {
			return err
		}
		clearedOptions := []*entity.GithubFile{}
		for _, file := range options {
			if file.Type == "file" {
				continue
			}
			clearedOptions = append(clearedOptions, file)
		}
		file, err := ui.PromptFiles(prompts[i], clearedOptions)
		if err != nil {
			return err
		}
		dirs = append(dirs, file.Name)
	}
	fmt.Println("Fetching Template...")
	err := exec.Command("git", "clone", "git@github.com:railwayapp/examples.git", "delete-me").Run()
	if err != nil {
		return err
	}
	fmt.Println("Generating Folder...")
	err = exec.Command("mv", fmt.Sprintf("delete-me/%s", strings.Join(dirs[:], "/")), ".").Run()
	if err != nil {
		return err
	}
	fmt.Println("Cleaning Up")
	err = exec.Command("rm", "-rf", "delete-me").Run()
	fmt.Println("Done 🎉")
	return err
}
