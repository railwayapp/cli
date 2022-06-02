package cmd

import (
	"context"
	"fmt"
	"io/ioutil"
	"time"

	"github.com/railwayapp/cli/entity"
	CLIErrors "github.com/railwayapp/cli/errors"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Up(ctx context.Context, req *entity.CommandRequest) error {
	var src string

	if len(req.Args) == 0 {
		src = "."
	} else {
		src = "./" + req.Args[0]
	}

	isVerbose, err := req.Cmd.Flags().GetBool("verbose")
	if err != nil {
		// Verbose mode isn't a necessary flag; just default to false.
		isVerbose = false
	}

	serviceName, err := req.Cmd.Flags().GetString("service")
	if err != nil {
		return err
	}

	fmt.Print(ui.VerboseInfo(isVerbose, "Using verbose mode"))

	fmt.Print(ui.VerboseInfo(isVerbose, "Loading project configuration"))
	projectConfig, err := h.ctrl.GetProjectConfigs(ctx)
	if err != nil {
		return err
	}

	fmt.Print(ui.VerboseInfo(isVerbose, "Loading environment"))
	environmentName, err := req.Cmd.Flags().GetString("environment")
	if err != nil {
		return err
	}

	environment, err := h.getEnvironment(ctx, environmentName)
	if err != nil {
		return err
	}
	fmt.Print(ui.VerboseInfo(isVerbose, fmt.Sprintf("Using environment %s", ui.Bold(environment.Name))))

	fmt.Print(ui.VerboseInfo(isVerbose, "Loading project"))
	project, err := h.ctrl.GetProject(ctx, projectConfig.Project)
	if err != nil {
		return err
	}

	serviceId := ""
	if serviceName != "" {
		for _, service := range project.Services {
			if service.Name == serviceName {
				serviceId = service.ID
			}
		}

		if serviceId == "" {
			return CLIErrors.ServiceNotFound
		}
	}

	// If service has not been provided via flag, prompt for it
	if serviceId == "" {
		fmt.Print(ui.VerboseInfo(isVerbose, "Loading services"))
		service, err := ui.PromptServices(project.Services)
		if err != nil {
			return err
		}

		if service != nil {
			serviceId = service.ID
		}
	}

	_, err = ioutil.ReadFile(".railwayignore")
	if err == nil {
		fmt.Print(ui.VerboseInfo(isVerbose, "Using ignore file .railwayignore"))
	}

	ui.StartSpinner(&ui.SpinnerCfg{
		Message: "Laying tracks in the clouds...",
	})
	res, err := h.ctrl.Upload(ctx, &entity.UploadRequest{
		ProjectID:     projectConfig.Project,
		EnvironmentID: environment.Id,
		ServiceID:     serviceId,
		RootDir:       src,
	})
	if err != nil {
		return err
	} else {
		ui.StopSpinner(fmt.Sprintf("☁️ Build logs available at %s\n", ui.GrayText(res.URL)))
	}
	detach, err := req.Cmd.Flags().GetBool("detach")
	if err != nil {
		return err
	}
	if detach {
		return nil
	}

	for i := 0; i < 3; i++ {
		err = h.ctrl.GetActiveBuildLogs(ctx, 0)
		if err == nil {
			break
		}
		time.Sleep(time.Duration(i) * 250 * time.Millisecond)
	}

	fmt.Printf("\n\n======= Build Completed ======\n\n")

	err = h.ctrl.GetActiveDeploymentLogs(ctx, 1000)
	if err != nil {
		return err
	}

	fmt.Printf("☁️ Deployment logs available at %s\n", ui.GrayText(res.URL))
	fmt.Printf("OR run `railway logs` to tail them here\n\n")

	if res.DeploymentDomain != "" {
		fmt.Printf("☁️ Deployment live at %s\n", ui.GrayText(h.ctrl.GetFullUrlFromStaticUrl(res.DeploymentDomain)))
	} else {
		fmt.Printf("☁️ Deployment is live\n")
	}

	return nil
}
