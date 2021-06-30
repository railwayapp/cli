package cmd

import (
	"context"
	"os"

	"github.com/railwayapp/cli/entity"
)

func (h *Handler) Completion(ctx context.Context, req *entity.CommandRequest) error {
	switch req.Args[0] {
	case "bash":
		err := req.Cmd.Root().GenBashCompletion(os.Stdout)
		if err != nil {
			return err
		}
	case "zsh":
		err := req.Cmd.Root().GenZshCompletion(os.Stdout)
		if err != nil {
			return err
		}
	case "fish":
		err := req.Cmd.Root().GenFishCompletion(os.Stdout, true)
		if err != nil {
			return err
		}
	case "powershell":
		err := req.Cmd.Root().GenPowerShellCompletion(os.Stdout)
		if err != nil {
			return err
		}
	}
	return nil
}
