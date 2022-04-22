package cmd

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"runtime"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Shell(ctx context.Context, req *entity.CommandRequest) error {
	serviceName, err := req.Cmd.Flags().GetString("service")
	if err != nil {
		return err
	}

	envs, err := h.ctrl.GetEnvsForService(ctx, &serviceName)
	if err != nil {
		return err
	}

	environment, err := h.ctrl.GetCurrentEnvironment(ctx)
	if err != nil {
		return err
	}

	shellVar := os.Getenv("SHELL")
	if shellVar == "" {
		// Fallback shell to use
		if isWindows() {
			shellVar = "cmd"
		} else {
			shellVar = "bash"
		}
	}

	fmt.Print(ui.Paragraph(fmt.Sprintf("Loading subshell with variables from %s", environment.Name)))

	subShellCmd := exec.CommandContext(ctx, shellVar)
	subShellCmd.Env = os.Environ()
	for k, v := range *envs {
		subShellCmd.Env = append(subShellCmd.Env, fmt.Sprintf("%s=%+v", k, v))
	}

	subShellCmd.Stdout = os.Stdout
	subShellCmd.Stderr = os.Stderr
	subShellCmd.Stdin = os.Stdin
	catchSignals(ctx, subShellCmd, nil)

	err = subShellCmd.Run()

	return err
}

func isWindows() bool {
	return runtime.GOOS == "windows"
}
