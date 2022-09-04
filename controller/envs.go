package controller

import (
	"context"
	"encoding/json"
	"fmt"
	"io/ioutil"
	"os"

	"github.com/joho/godotenv"
	"github.com/railwayapp/cli/entity"
	CLIErrors "github.com/railwayapp/cli/errors"
	"github.com/railwayapp/cli/ui"
)

func (c *Controller) GetEnvsForCurrentEnvironment(ctx context.Context, serviceName *string) (*entity.Envs, error) {
	environment, err := c.GetCurrentEnvironment(ctx)
	if err != nil {
		return nil, err
	}

	return c.GetEnvs(ctx, environment, serviceName)
}

func (c *Controller) GetEnvs(ctx context.Context, environment *entity.Environment, serviceName *string) (*entity.Envs, error) {
	projectCfg, err := c.GetProjectConfigs(ctx)
	if err != nil {
		return nil, err
	}

	project, err := c.GetCurrentProject(ctx)
	if err != nil {
		return nil, err
	}

	// Get service id from name
	serviceId := ""

	if serviceName != nil && *serviceName != "" {
		for _, service := range project.Services {
			if service.Name == *serviceName {
				serviceId = service.ID
			}
		}

		if serviceId == "" {
			return nil, CLIErrors.ServiceNotFound
		}
	}

	if serviceId == "" {
		service, err := ui.PromptServices(project.Services)
		if err != nil {
			return nil, err
		}

		if service != nil {
			serviceId = service.ID
		}
	}

	if val, ok := projectCfg.LockedEnvsNames[environment.Id]; ok && val {
		fmt.Println(ui.Bold(ui.RedText("Protected Environment Detected!").String()))
		confirm, err := ui.PromptYesNo("Continue fetching variables?")
		if err != nil {
			return nil, err
		}
		if !confirm {
			return nil, nil
		}
	}

	return c.gtwy.GetEnvs(ctx, &entity.GetEnvsRequest{
		ProjectID:     projectCfg.Project,
		EnvironmentID: environment.Id,
		ServiceID:     serviceId,
	})
}

func (c *Controller) AutoImportDotEnv(ctx context.Context) error {
	dir, err := os.Getwd()
	if err != nil {
		return err
	}

	envFileLocation := fmt.Sprintf("%s/.env", dir)
	if _, err := os.Stat(envFileLocation); err == nil {
		// path/to/whatever does not exist
		shouldImportEnvs, err := ui.PromptYesNo("\n.env detected!\nImport your variables into Railway?")
		if err != nil {
			return err
		}
		// If the user doesn't want to import envs skip
		if !shouldImportEnvs {
			return nil
		}
		// Otherwise read .env and set envs
		err = godotenv.Load()
		if err != nil {
			return err
		}
		envMap, err := godotenv.Read()
		if err != nil {
			return err
		}
		if len(envMap) > 0 {
			return c.UpdateEnvs(ctx, (*entity.Envs)(&envMap), nil, false)
		}
	}
	return nil
}

func (c *Controller) SaveEnvsToFile(ctx context.Context) error {
	envs, err := c.GetEnvsForCurrentEnvironment(ctx, nil)
	if err != nil {
		return err
	}

	err = c.cfg.CreatePathIfNotExist(c.cfg.RailwayEnvFilePath)
	if err != nil {
		return err
	}

	encoded, err := json.MarshalIndent(envs, "", "  ")
	if err != nil {
		return err
	}

	err = ioutil.WriteFile(c.cfg.RailwayEnvFilePath, encoded, os.ModePerm)
	if err != nil {
		return err
	}

	return nil
}

func (c *Controller) UpdateEnvs(ctx context.Context, envs *entity.Envs, serviceName *string, replace bool) error {
	projectCfg, err := c.GetProjectConfigs(ctx)
	if err != nil {
		return err
	}

	err = c.PromptIfProtectedEnvironment(ctx)
	if err != nil {
		return err
	}

	project, err := c.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return err
	}

	// Get service id from name
	serviceID := ""
	if serviceName != nil && *serviceName != "" {
		for _, service := range project.Services {
			if service.Name == *serviceName {
				serviceID = service.ID
			}
		}

		if serviceID == "" {
			return CLIErrors.ServiceNotFound
		}
	}

	if serviceID == "" {
		service, err := ui.PromptServices(project.Services)
		if err != nil {
			return err
		}
		if service != nil {
			serviceID = service.ID
		}
	}

	pluginID := ""

	// If there is no service, use the env plugin
	if serviceID == "" {
		for _, p := range project.Plugins {
			if p.Name == "env" {
				pluginID = p.ID
			}
		}
	}

	return c.gtwy.UpdateVariablesFromObject(ctx, &entity.UpdateEnvsRequest{
		ProjectID:     projectCfg.Project,
		EnvironmentID: projectCfg.Environment,
		PluginID:      pluginID,
		ServiceID:     serviceID,
		Envs:          envs,
		Replace:       replace,
	})
}

func (c *Controller) DeleteEnvs(ctx context.Context, names []string, serviceName *string) error {
	projectCfg, err := c.GetProjectConfigs(ctx)
	if err != nil {
		return err
	}

	err = c.PromptIfProtectedEnvironment(ctx)
	if err != nil {
		return err
	}

	project, err := c.GetProject(ctx, projectCfg.Project)
	if err != nil {
		return err
	}

	// Get service id from name
	serviceID := ""
	if serviceName != nil && *serviceName != "" {
		for _, service := range project.Services {
			if service.Name == *serviceName {
				serviceID = service.ID
			}
		}

		if serviceID == "" {
			return CLIErrors.ServiceNotFound
		}
	}

	if serviceID == "" {
		service, err := ui.PromptServices(project.Services)
		if err != nil {
			return err
		}
		if service != nil {
			serviceID = service.ID
		}
	}

	pluginID := ""

	// If there is no service, use the env plugin
	if serviceID == "" {
		for _, p := range project.Plugins {
			if p.Name == "env" {
				pluginID = p.ID
			}
		}
	}

	// Delete each variable one by one
	for _, name := range names {
		err = c.gtwy.DeleteVariable(ctx, &entity.DeleteVariableRequest{
			ProjectID:     projectCfg.Project,
			EnvironmentID: projectCfg.Environment,
			PluginID:      pluginID,
			ServiceID:     serviceID,
			Name:          name,
		})

		if err != nil {
			return err
		}
	}

	return nil
}
