package cmd

import (
	"context"
	"fmt"
	"net"
	"os"
	"os/exec"
	"os/signal"
	"regexp"
	"syscall"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/errors"
	"github.com/railwayapp/cli/ui"
)

var RAIL_PORT = 4411

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

	cmd := exec.CommandContext(ctx, req.Args[0], req.Args[1:]...)
	cmd.Env = os.Environ()

	// Inject railway envs
	for k, v := range *envs {
		cmd.Env = append(cmd.Env, fmt.Sprintf("%s=%+v", k, v))
	}

	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stdout
	cmd.Stdin = os.Stdin
	catchSignals(cmd)

	err = cmd.Run()

	if err != nil {
		if exitError, ok := err.(*exec.ExitError); ok {
			os.Exit(exitError.ExitCode())
		}

		os.Exit(1)
	}

	printLooksGood()

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
	environment, err := h.ctrl.GetEnvironment(ctx)
	if err != nil {
		return err
	}

	sanitiser := regexp.MustCompile(`[^A-Za-z0-9_-]`)
	imageNameWithoutNsOrTag := sanitiser.ReplaceAllString(project.Name, "") + "-" + sanitiser.ReplaceAllString(environment.Name, "")
	image := fmt.Sprintf("railway-local/%s:latest", imageNameWithoutNsOrTag)

	buildArgs := []string{"build", "-q", "-t", image, pwd}

	// Build up env
	for k, v := range *envs {
		buildArgs = append(buildArgs, "--build-arg", fmt.Sprintf("%s=\"%+v\"", k, v))
	}

	buildCmd := exec.CommandContext(ctx, "docker", buildArgs...)
	ui.StartSpinner(&ui.SpinnerCfg{
		Message: fmt.Sprintf("Building %s from Dockerfile... (this may take a bit)", ui.GreenText(image)),
		Tokens:  ui.TrainEmojis,
	})

	out, err := buildCmd.CombinedOutput()
	if err != nil {
		ui.StopSpinner("")
		return showCmdError(buildCmd.Args, out, err)
	}

	ui.StopSpinner(fmt.Sprintf("🎉 Built %s", ui.GreenText(image)))

	port, err := getAvailablePort()
	if err != nil {
		return err
	}
	// Start running the image
	fmt.Printf("🚂 Running at %s\n\n", ui.GreenText(fmt.Sprintf("127.0.0.1:%d", port)))

	runArgs := []string{"run", "--init", "--rm", "-p", fmt.Sprintf("127.0.0.1:%d:%d", port, port), "-e", fmt.Sprintf("PORT=%d", port)}
	// Build up env
	for k, v := range *envs {
		runArgs = append(runArgs, "-e", fmt.Sprintf("%s=%+v", k, v))
	}
	runArgs = append(runArgs, image)

	// Run the container
	runCmd := exec.CommandContext(ctx, "docker", runArgs...)
	runCmd.Stdout = os.Stdout
	runCmd.Stderr = os.Stdout
	runCmd.Stdin = os.Stdin
	catchSignals(runCmd)

	err = runCmd.Run()

	if err != nil {
		return err
	}

	printLooksGood()

	return nil
}

func getAvailablePort() (int, error) {
	searchRange := 64
	for i := RAIL_PORT; i < RAIL_PORT+searchRange; i++ {
		if isAvailable(i) {
			return i, nil
		}
	}
	return -1, fmt.Errorf("Couldn't find available port between %d and %d", RAIL_PORT, RAIL_PORT+searchRange)
}

func isAvailable(port int) bool {
	ln, err := net.Listen("tcp", fmt.Sprintf(":%d", port))
	if err != nil {
		return false
	}
	_ = ln.Close()
	return true
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

func catchSignals(cmd *exec.Cmd) {
	sigs := make(chan os.Signal, 1)

	signal.Notify(sigs, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		sig := <-sigs
		err := cmd.Process.Signal(sig)
		if err != nil {
			fmt.Println("Child process error: \n", err)
		}
	}()
}

func printLooksGood() {
	// Get space between last output and this message
	fmt.Println()
	fmt.Printf(
		"🚄 Looks good? Then put it on the train and deploy with `%s`!\n",
		ui.GreenText("railway up"),
	)
}
