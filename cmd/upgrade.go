package cmd

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/railwayapp/cli/constants"
	"github.com/railwayapp/cli/entity"
)

var OriginalInstallationMethod string

func getExectuablePath() string {
	ex, err := os.Executable()
	if err != nil {
		panic(err)
	}
	exPath := filepath.Dir(ex)
	return exPath
}

func getUpdateCommand(installationMethod string) (*exec.Cmd, error) {
	if installationMethod == "brew" {
		return exec.Command("brew", "upgrade", "railway"), nil
	} else if installationMethod == "curl" {
		return exec.Command("curl", "-fsSL", "https://railway.app/install.sh", "|", "sh"), nil
	} else if installationMethod == "npm" {
		currentPath := getExectuablePath()
		if strings.Contains(currentPath, "npm") {
			return exec.Command("npm", "i", "-g", "@railway/cli"), nil
		} else if strings.Contains(currentPath, "yarn") {
			return exec.Command("yarn", "global", "add", "@railway/cli"), nil
		}
	}
	return nil, errors.New("installation method not recognized")
}

func (h *Handler) Upgrade(ctx context.Context, req *entity.CommandRequest) error {
	latestVersion, _ := h.ctrl.GetLatestVersion()
	if latestVersion == constants.Version {
		fmt.Printf("\nYou are currently up to date")
		return nil
	}

	updateCommand, e := getUpdateCommand(OriginalInstallationMethod)
	fmt.Println(updateCommand)
	if e != nil {
		fmt.Printf(e.Error())
		return nil
	}

	if h.ctrl.RunUpdateCommand(updateCommand) == nil {
		fmt.Printf("Error when we try to run upload command")
		return nil
	}
	fmt.Printf("upload run sucressfuly")

	return nil
}
