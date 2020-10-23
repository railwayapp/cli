package ui

import (
	"fmt"

	"github.com/manifoldco/promptui"
	"github.com/railwayapp/cli/entity"
)

type Prompt string
type Selection string

const (
	InitPrompt      Prompt    = "What would you like to do?"
	InitNew         Selection = "Create new Project"
	InitFromAccount Selection = "Connect to existing project"
	InitFromID      Selection = "Enter existing project id"
)

func PromptInit(isLoggedIn bool) (Selection, error) {
	existingProjectPrompt := InitFromID
	if isLoggedIn {
		existingProjectPrompt = InitFromAccount
	}
	selectPrompt := promptui.Select{
		Label: InitPrompt,
		Items: []Selection{InitNew, existingProjectPrompt},
	}
	_, selection, err := selectPrompt.Run()
	return Selection(selection), err
}

func PromptText(text string) (string, error) {
	prompt := promptui.Prompt{
		Label: text,
	}
	return prompt.Run()
}

func PromptProjects(projects []*entity.Project) (*entity.Project, error) {
	prompt := promptui.Select{
		Label: "Select Project",
		Items: projects,
		Templates: &promptui.SelectTemplates{
			Active:   `{{ .Name | underline }}`,
			Inactive: `{{ .Name }}`,
			Selected: fmt.Sprintf("%s Project: {{ .Name | magenta | bold }} ", GreenText("✔")),
		},
	}
	i, _, err := prompt.Run()
	return projects[i], err
}

func PromptEnvironments(environments []*entity.Environment) (*entity.Environment, error) {
	prompt := promptui.Select{
		Label: "Select Environment",
		Items: environments,
		Templates: &promptui.SelectTemplates{
			Active:   `{{ .Name | underline }}`,
			Inactive: `{{ .Name }}`,
			Selected: fmt.Sprintf("%s Environment: {{ .Name | blue | bold }} ", GreenText("✔")),
		},
	}
	i, _, err := prompt.Run()
	return environments[i], err
}

func PromptPlugins(plugins []*entity.Plugin) (*entity.Plugin, error) {
	prompt := promptui.Select{
		Label: "Select Plugin",
		Items: plugins,
		Templates: &promptui.SelectTemplates{
			Active:   `{{ .Name | underline }}`,
			Inactive: `{{ .Name }}`,
			Selected: fmt.Sprintf("%s Plugin: {{ .Name | magenta | bold }} ", GreenText("✔")),
		},
	}
	i, _, err := prompt.Run()
	return plugins[i], err
}
