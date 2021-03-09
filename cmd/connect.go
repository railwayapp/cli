package cmd

import (
	"context"
	"fmt"
	"os"
	"os/exec"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Connect(ctx context.Context, req *entity.CommandRequest) error {
	projectCfg, _ := h.cfg.GetProjectConfigs()

	project, err := h.ctrl.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return err
	}

	envName := getEnvironmentNameFromID(projectCfg.Environment, project.Environments)

	fmt.Printf("🎉 Connecting to: %s %s\n", ui.MagentaText(project.Name), ui.MagentaText(envName))

	names := make([]string, 0)
	for _, plugin := range project.Plugins {
		// TODO: Better way of handling this
		if plugin.Name != "env" {
			names = append(names, plugin.Name)
		}
	}

	fmt.Println("Select a database to connect to:")
	plugin, err := ui.PromptPlugins(names)
	if err != nil {
		return err
	}

	envs, err := h.ctrl.GetEnvs(ctx)
	if err != nil {
		return err
	}

	var command []string
	var connectEnv map[string]string
	switch plugin {
	case "redis":
		// run
		command = []string{"redis-cli", "-u", (*envs)["REDIS_URL"]}
		break

	case "postgresql":
		connectEnv = map[string]string{
			"PGPASSWORD": (*envs)["PGPASSWORD"],
		}
		command = []string{
			"psql",
			"-U",
			(*envs)["PGUSER"],
			"-h",
			(*envs)["PGHOST"],
			"-p",
			(*envs)["PGPORT"],
			"-d",
			(*envs)["PGDATABASE"],
		}
		break

	case "mongodb":
		command = []string{
			"mongo",
			fmt.Sprintf(
				"mongodb://%s:%s@%s:%s",
				(*envs)["MONGOUSER"],
				(*envs)["MONGOPASSWORD"],
				(*envs)["MONGOHOST"],
				(*envs)["MONGOPORT"],
			),
		}
		break

	case "mysql":

		command = []string{
			"mysql",
			fmt.Sprintf("-h%s", (*envs)["MYSQLHOST"]),
			fmt.Sprintf("-u%s", (*envs)["MYSQLUSER"]),
			fmt.Sprintf("-p%s", (*envs)["MYSQLPASSWORD"]),
			fmt.Sprintf("--port=%s", (*envs)["MYSQLPORT"]),
			"--protocol=TCP",
			(*envs)["MYSQLDATABASE"],
		}
		break
	}

	cmd := exec.Command(command[0], command[1:]...)

	cmd.Env = os.Environ()
	for k, v := range connectEnv {
		cmd.Env = append(cmd.Env, fmt.Sprintf("%s=%+v", k, v))
	}

	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stdout
	cmd.Stdin = os.Stdin
	catchSignals(cmd)

	err = cmd.Run()
	if err != nil {
		return err
	}

	return nil
}
