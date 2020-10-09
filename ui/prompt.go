package ui

import (
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
			Selected: `{{ .Name }}`,
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
			Selected: `{{ .Name }}`,
		},
	}
	i, _, err := prompt.Run()
	return environments[i], err
}

func PromptTemplate() (string, error) {
	promptLang := promptui.Select{
		Label: "Select Language",
		Items: []string{"JavaScript", "Python", "Ruby", "Elixir"},
	}

	_, _, err := promptLang.Run()
	if err != nil {
		return "", err
	}

	promptType := promptui.Select{
		Label: "Select Language",
		Items: []string{"Basic", "Blog", "Todo"},
	}

	_, _, err = promptType.Run()
	if err != nil {
		return "", err
	}
	promptProj := promptui.Select{
		Label: "Select Template",
		Items: []string{"Redwood", "NextJS", "Ghost"},
	}
	_, selection, err := promptProj.Run()
	return selection, err
}

func PromptFiles(label string, files []*entity.GithubFile) (*entity.GithubFile, error) {
	prompt := promptui.Select{
		Label: label,
		Items: files,
		Templates: &promptui.SelectTemplates{
			Active:   `{{ .Name | underline }}`,
			Inactive: `{{ .Name }}`,
			Selected: `{{ .Name }}`,
		},
	}
	i, _, err := prompt.Run()
	return files[i], err
}
