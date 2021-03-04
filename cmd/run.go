package cmd

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"regexp"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Run(ctx context.Context, req *entity.CommandRequest) error {
	envs, err := h.ctrl.GetEnvs(ctx)

	if err != nil {
		return err
	}

	pwd, err := os.Getwd()
	if err != nil {
		return err
	}

	hasDockerfile := true

	if _, err := os.Stat(fmt.Sprintf("%s/Dockerfile", pwd)); os.IsNotExist(err) {
		hasDockerfile = false
	}

	if len(req.Args) == 0 && hasDockerfile {
		return h.runInDocker(ctx, pwd, envs)
	} else if len(req.Args) == 0 {
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

func (h *Handler) runInDocker(ctx context.Context, pwd string, envs *entity.Envs) error {
	// Start building the image
	projectCfg, err := h.cfg.GetProjectConfigs()
	if err != nil {
		return err
	}

	project, err := h.ctrl.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return err
	}

	// Strip characters not allowed in Docker image names
	sanitiser := regexp.MustCompile(`[^A-Za-z0-9_-]`)
	imageName := sanitiser.ReplaceAllString(project.Name, "")
	image := fmt.Sprintf("railway-local/%s:latest", imageName)

	buildArgs := []string{"build", "-q", "-t", image, pwd}

	// Build up env
	for k, v := range *envs {
		buildArgs = append(buildArgs, "--build-arg", fmt.Sprintf("%s=\"%+v\"", k, v))
	}

	buildCmd := exec.Command("docker", buildArgs...)
	ui.StartSpinner(&ui.SpinnerCfg{
		Message: fmt.Sprintf("Building %s from Dockerfile...", image),
		Tokens:  ui.TrainEmojis,
	})

	out, err := buildCmd.CombinedOutput()
	if err != nil {
		ui.StopSpinner("")
		return showCmdError(buildCmd.Args, out, err)
	}

	ui.StopSpinner(fmt.Sprintf("🎉 Built %s", image))

	port := 4411
	// Start running the image
	fmt.Printf("🚅 Running %s at 127.0.0.1:%d\n\n", image, port)

	runArgs := []string{"run", "-p", fmt.Sprintf("127.0.0.1:%d:%d", port, port), "-e", fmt.Sprintf("PORT=%d", port)}
	// Build up env
	for k, v := range *envs {
		runArgs = append(runArgs, "-e", fmt.Sprintf("%s=%+v", k, v))
	}
	runArgs = append(runArgs, image)

	// Run the container
	runCmd := exec.Command("docker", runArgs...)
	runCmd.Stdout = os.Stdout
	runCmd.Stderr = os.Stdout
	runCmd.Stdin = os.Stdin

	err = runCmd.Run()
	if err != nil {
		return err
	}

	// TODO: Probably should be cleaning up the image here...

	return nil
}

func showCmdError(args []string, output []byte, err error) error {
	if _, ok := err.(*exec.ExitError); ok {
		// Full cmd for error logging
		argstr := ""
		for _, arg := range args {
			argstr += arg + " "
		}

		fmt.Println(ui.RedText("exec error:"))
		fmt.Println(ui.RedText("-- START OUTPUT --"))
		fmt.Printf("%s\n", string(output))
		fmt.Println(ui.RedText("-- END OUTPUT --"))
		fmt.Println()
		fmt.Println(ui.RedText("while running:"))
		fmt.Printf("%+v\n", argstr)
	}
	return err
}
