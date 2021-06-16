package controller

import (
	"context"
	"fmt"
	"os"

	"github.com/railwayapp/cli/constants"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (c *Controller) SendPanic(ctx context.Context, panicErr string, stacktrace string, command string) (bool, error) {
	confirmSendPanic()

	projectCfg, err := c.cfg.GetProjectConfigs()
	if err != nil {
		return c.gtwy.SendPanic(ctx, &entity.PanicRequest{
			Command:       command,
			PanicError:    panicErr,
			Stacktrace:    stacktrace,
			ProjectID:     "",
			EnvironmentID: "",
			Version:       constants.Version,
		})

	}
	return c.gtwy.SendPanic(ctx, &entity.PanicRequest{
		Command:       command,
		PanicError:    panicErr,
		Stacktrace:    stacktrace,
		ProjectID:     projectCfg.Project,
		EnvironmentID: projectCfg.Environment,
		Version:       constants.Version,
	})

}

func confirmSendPanic() {
	fmt.Printf("🚨 Looks like something derailed, Press Enter to send error logs (^C to quit)")
	fmt.Fscanln(os.Stdin)
	ui.StartSpinner(&ui.SpinnerCfg{
		Message: "Taking notes...",
	})
}
