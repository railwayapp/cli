package controller

import (
	"context"
	"encoding/json"
	"fmt"
	"io/ioutil"
	"os"

	"github.com/joho/godotenv"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (c *Controller) GetEnvs(ctx context.Context) (*entity.Envs, error) {
	projectCfg, err := c.GetProjectConfigs(ctx)
	if err != nil {
		return nil, err
	}

	project, err := c.GetCurrentProject(ctx)
	if err != nil {
		return nil, err
	}

	service, err := ui.PromptServices(project.Services)
	if err != nil {
		return nil, err
	}

	if val, ok := projectCfg.LockedEnvsNames[projectCfg.Environment]; ok && val {
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
		EnvironmentID: projectCfg.Environment,
		ServiceID:     service.ID,
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
			return c.UpsertEnvsForEnvPlugin(ctx, (*entity.Envs)(&envMap))
		}
	}
	return nil
}

func (c *Controller) SaveEnvsToFile(ctx context.Context) error {
	envs, err := c.GetEnvs(ctx)
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

func (c *Controller) UpsertEnvsForEnvPlugin(ctx context.Context, envs *entity.Envs) error {
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

	pluginID := ""
	for _, p := range project.Plugins {
		if p.Name == "env" {
			pluginID = p.ID
		}
	}

	return c.gtwy.UpsertVariablesFromObject(ctx, &entity.UpdateEnvsRequest{
		ProjectID:     projectCfg.Project,
		EnvironmentID: projectCfg.Environment,
		PluginID:      pluginID,
		Envs:          envs,
	})
}

func (c *Controller) DeleteEnvsForEnvPlugin(ctx context.Context, names []string) error {
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

	pluginID := ""
	for _, p := range project.Plugins {
		if p.Name == "env" {
			pluginID = p.ID
		}
	}

	// Delete each variable one by one
	for _, name := range names {
		err = c.gtwy.DeleteVariable(ctx, &entity.DeleteVariableRequest{
			ProjectID:     projectCfg.Project,
			EnvironmentID: projectCfg.Environment,
			PluginID:      pluginID,
			Name:          name,
		})

		if err != nil {
			return err
		}
	}

	return nil
}
