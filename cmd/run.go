package cmd

import (
	"context"
	"fmt"
	"os"
	"os/exec"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
)

func (h *Handler) Run(ctx context.Context, req *entity.CommandRequest) error {
	envs, err := h.ctrl.GetEnvs(ctx)

	if err != nil {
		return err
	}

	if len(req.Args) == 0 {
		return errors.CommandNotSpecified
	}

	cmd := exec.Command(req.Args[0], req.Args[1:]...)
	cmd.Env = os.Environ()

	// Inject railway envs
	for k, v := range *envs {
		cmd.Env = append(cmd.Env, fmt.Sprintf("%s=%+v", k, v))
	}

	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stdout
	cmd.Stdin = os.Stdin

	err = cmd.Run()
	if err != nil {
		return err
	}

	return nil
}
