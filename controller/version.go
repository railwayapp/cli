package controller

import (
	"context"
	"fmt"
	"os/exec"
)

func (c *Controller) GetLatestVersion() (string, error) {
	rep, _, err := c.ghc.Repositories.GetLatestRelease(context.Background(), "railwayapp", "cli")
	if err != nil {
		return "", err
	}
	return *rep.TagName, nil
}
func (c *Controller) RunUpdateCommand(updateCommand *exec.Cmd) error {
	err := updateCommand.Run()
	fmt.Println(err)
	return err
}
