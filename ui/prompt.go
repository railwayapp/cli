package ui

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"

	"github.com/manifoldco/promptui"
	"github.com/railwayapp/cli/entity"
)

type Prompt string
type Selection string

const (
	InitPrompt       Prompt    = "What would you like to do?"
	InitNew          Selection = "Create new Project"
	InitFromTemplate Selection = "Deploy a starter project"
	InitFromAccount  Selection = "Connect to existing project"
	InitFromID       Selection = "Enter existing project id"
)

func PromptInit(isLoggedIn bool) (Selection, error) {
	existingProjectPrompt := InitFromID
	if isLoggedIn {
		existingProjectPrompt = InitFromAccount
	}
	selectPrompt := promptui.Select{
		Label: InitPrompt,
		Items: []Selection{InitNew, existingProjectPrompt, InitFromTemplate},
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
			Selected: fmt.Sprintf("%s Project: {{ .Name | magenta | bold }} ", promptui.IconGood),
		},
	}
	i, _, err := prompt.Run()
	return projects[i], err
}

// PromptStarterTemplates fetches available templates and prompts the user to select one
func PromptStarterTemplates() (*entity.Template, error) {
	StartSpinner(&SpinnerCfg{
		Message: "Fetching starter templates",
	})
	resp, err := http.Get("https://raw.githubusercontent.com/railwayapp/starters/master/featured.json")
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	var data struct {
		Templates []entity.Template `json:"examples"`
	}
	dec := json.NewDecoder(resp.Body)
	err = dec.Decode(&data)
	if err != nil {
		return nil, err
	}

	StopSpinner("")

	prompt := promptui.Select{
		Label: "Select Starter Template",
		Items: data.Templates,
		Templates: &promptui.SelectTemplates{
			Active:   fmt.Sprintf("%s {{ .Text | underline }}", promptui.IconSelect),
			Inactive: `  {{ .Text }}`,
			Selected: fmt.Sprintf("%s Template: {{ .Text | magenta | bold }} ", GreenText("✔")),
		},
	}
	i, _, err := prompt.Run()
	return &data.Templates[i], err
}

func PromptIsRepoPrivate() (bool, error) {
	prompt := promptui.Select{
		Label: "Select repo visibility",
		Items: []string{"Public", "Private"},
	}
	_, visibility, err := prompt.Run()
	return visibility == "Private", err
}

func PromptProjectName() (string, error) {
	prompt := promptui.Prompt{
		Label: "Enter project name",
		Templates: &promptui.PromptTemplates{
			Prompt:          "{{ . }} ",
			Confirm:         "",
			Valid:           fmt.Sprintf("%s {{ . | bold }}: ", promptui.IconGood),
			Invalid:         fmt.Sprintf("%s {{ . | bold }}: ", promptui.IconBad),
			Success:         fmt.Sprintf("%s {{ . | magenta | bold }}: ", promptui.IconGood),
			ValidationError: "",
			FuncMap:         nil,
		},
		Validate: func(s string) error {
			if s == "" {
				return errors.New("project name required")
			}
			return nil
		},
	}
	return prompt.Run()
}

// PromptGitHubScopes prompts the user to select one of the provides scopes
func PromptGitHubScopes(scopes []string) (string, error) {
	if len(scopes) == 1 {
		return scopes[0], nil
	}

	prompt := promptui.Select{
		Label: "Select GitHub Owner",
		Items: scopes,
		Templates: &promptui.SelectTemplates{
			Active:   fmt.Sprintf("%s {{ . | underline }}", promptui.IconSelect),
			Inactive: `  {{ . }}`,
			Selected: fmt.Sprintf("%s GitHub: {{ . | magenta | bold }} ", GreenText("✔")),
		},
	}
	_, scope, err := prompt.Run()
	return scope, err
}

func PromptEnvironments(environments []*entity.Environment) (*entity.Environment, error) {
	if len(environments) == 1 {
		environment := environments[0]
		fmt.Printf("%s Environment: %s\n", promptui.IconGood, BlueText(environment.Name))
		return environment, nil
	}
	prompt := promptui.Select{
		Label: "Select Environment",
		Items: environments,
		Templates: &promptui.SelectTemplates{
			Active:   `{{ .Name | underline }}`,
			Inactive: `{{ .Name }}`,
			Selected: fmt.Sprintf("%s Environment: {{ .Name | blue | bold }} ", promptui.IconGood),
		},
	}
	i, _, err := prompt.Run()
	return environments[i], err
}

func PromptPlugins(plugins []string) (string, error) {
	prompt := promptui.Select{
		Label: "Select Plugin",
		Items: plugins,
		Templates: &promptui.SelectTemplates{
			Active:   `{{ . | underline }}`,
			Inactive: `{{ . }}`,
			Selected: fmt.Sprintf("%s Plugin: {{ . | blue | bold }} ", promptui.IconGood),
		},
	}
	i, _, err := prompt.Run()
	return plugins[i], err
}

func PromptConfirm(msg string) error {
	prompt := promptui.Prompt{
		Label: msg,
	}
	_, err := prompt.Run()
	return err
}
