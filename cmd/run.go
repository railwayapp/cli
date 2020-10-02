package cmd

import (
	"bufio"
	"context"
	"fmt"
	"os/exec"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Run(ctx context.Context, req *entity.CommandRequest) error {
	envs, err := h.ctrl.GetEnvs(ctx)

	argsString := ""

	// Inject railway envs
	for k, v := range *envs {
		argsString += fmt.Sprintf("%s=%+v ", k, v)
	}

	for _, arg := range req.Args {
		argsString += fmt.Sprintf("%s ", arg)
	}

	bashCommand := exec.Command("bash", "-c", argsString)

	pipe, _ := bashCommand.StdoutPipe()
	if err := bashCommand.Start(); err != nil {
		// handle error
	}
	reader := bufio.NewReader(pipe)
	line, err := reader.ReadString('\n')
	for err == nil {
		fmt.Println(line)
		line, err = reader.ReadString('\n')
	}
	return nil
}
