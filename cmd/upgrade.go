package cmd

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/railwayapp/cli/constants"
	"github.com/railwayapp/cli/entity"
)

func getExectuablePath() string {
	ex, err := os.Executable()
	if err != nil {
		panic(err)
	}
	exPath := filepath.Dir(ex)
	return exPath
}

func getUpdateCommand() *exec.Cmd {
	currentPath := getExectuablePath()

	// TODO: add brew support
	if strings.Contains(currentPath, "npm") {
		return exec.Command("npm", "i", "-g", "@railway/cli")
	} else if strings.Contains(currentPath, "yarn") {
		return exec.Command("yarn", "global", "add", "@railway/cli")
	} else {
		return exec.Command("curl", "-fsSL", "https://railway.app/install.sh", "|", "sh")
	}
}

func (h *Handler) Upgrade(ctx context.Context, req *entity.CommandRequest) error {
	latestVersion, _ := h.ctrl.GetLatestVersion()
	if latestVersion == constants.Version {
		fmt.Printf("\nYou are currently up to date")
		return nil
	}

	updateCommand := getUpdateCommand()
	fmt.Println(updateCommand)

	if h.ctrl.RunUpdateCommand(updateCommand) != nil {
		fmt.Printf("Error when we try to run upgrade command")
		return nil
	}
	fmt.Printf("Upgrade run sucressfuly")

	return nil
}
