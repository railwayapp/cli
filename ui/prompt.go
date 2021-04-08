package ui

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"

	"github.com/manifoldco/promptui"
	"github.com/railwayapp/cli/entity"
)

type Prompt string
type Selection string

const (
	InitPrompt       Prompt    = "What would you like to do?"
	InitNew          Selection = "Create new Project"
	InitFromTemplate Selection = "Select starter template"
)

func PromptInit() (Selection, error) {
	selectPrompt := promptui.Select{
		Label: InitPrompt,
		Items: []Selection{InitNew, InitFromTemplate},
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

func hasTeams(projects []*entity.Project) bool {
	teamKeys := make(map[string]bool)
	teams := []string{}

	for _, project := range projects {
		if project.Team != nil {
			if _, value := teamKeys[*project.Team]; !value {
				teamKeys[*project.Team] = true
				teams = append(teams, *project.Team)
			}
		}
	}

	return len(teams) > 1
}

func promptTeams(projects []*entity.Project) (*string, error) {
	if hasTeams(projects) {
		tm := make(map[string]bool)
		for _, project := range projects {
			if project.Team != nil {
				tm[*project.Team] = true
			}
		}
		teams := make([]string, 0)
		for team := range tm {
			teams = append(teams, team)
		}
		prompt := promptui.Select{
			Label: "Select Team",
			Items: teams,
			Templates: &promptui.SelectTemplates{
				Selected: fmt.Sprintf("%s Team: {{ .Name | green | bold }} ", promptui.IconGood),
			},
		}
		_, team, err := prompt.Run()
		return &team, err
	}

	return nil, nil
}

func PromptProjects(projects []*entity.Project) (*entity.Project, error) {
	// Check if need to prompt teams
	team, err := promptTeams(projects)
	if err != nil {
		return nil, err
	}
	filteredProjects := make([]*entity.Project, 0)

	if team == nil {
		filteredProjects = projects
	} else {
		for _, project := range projects {
			if *project.Team == *team {
				filteredProjects = append(filteredProjects, project)
			}
		}
	}

	prompt := promptui.Select{
		Label: "Select Project",
		Items: filteredProjects,
		Templates: &promptui.SelectTemplates{
			Active:   `{{ .Name | underline }}`,
			Inactive: `{{ .Name }}`,
			Selected: fmt.Sprintf("%s Project: {{ .Name | magenta | bold }} ", promptui.IconGood),
		},
	}
	i, _, err := prompt.Run()
	return filteredProjects[i], err
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

func PromptEnvVars(envVars []entity.TemplateEnvVar) (map[string]string, error) {
	variables := make(map[string]string)
	if len(envVars) > 0 {
		fmt.Printf("\n%s\n", Bold("Environment Variables"))
	}

	for _, envVar := range envVars {
		prompt := promptui.Prompt{
			Label:   envVar.Name,
			Default: envVar.DefaultValue,
		}
		if envVar.Optional {
			fmt.Printf("\n%s %s\n", envVar.Desc, GrayText("(Optional)"))
		} else {
			fmt.Printf("\n%s %s\n", envVar.Desc, GrayText("(Required)"))
			prompt.Validate = validatorRequired("value required")
		}

		v, err := prompt.Run()
		if err != nil {
			return nil, err
		}

		variables[envVar.Name] = v
	}

	// Extra newline to match the ones outputted in the loop
	fmt.Print("\n")

	return variables, nil
}

func PromptProjectName() (string, error) {
	prompt := promptui.Prompt{
		Label: "Enter project name",
		Templates: &promptui.PromptTemplates{
			Prompt:  "{{ . }} ",
			Valid:   fmt.Sprintf("%s {{ . | bold }}: ", promptui.IconGood),
			Invalid: fmt.Sprintf("%s {{ . | bold }}: ", promptui.IconBad),
			Success: fmt.Sprintf("%s {{ . | magenta | bold }}: ", promptui.IconGood),
		},
		Validate: validatorRequired("project name required"),
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

// PromptYesNo prompts the user to continue an action using the common (y/N) action
func PromptYesNo(msg string) (bool, error) {
	fmt.Printf("%s (y/N): ", msg)
	var response string
	_, err := fmt.Scan(&response)
	if err != nil {
		return false, err
	}
	response = strings.ToLower(response)

	isNo := response == "n" || response == "no"
	isYes := response == "y" || response == "yes"

	if isYes {
		return true, nil
	} else if isNo {
		return false, nil
	} else {
		fmt.Println("Please type yes or no and then press enter:")
		return PromptYesNo(msg)
	}
}

func validatorRequired(errorMsg string) func(s string) error {
	return func(s string) error {
		if strings.TrimSpace(s) == "" {
			return errors.New(errorMsg)
		}
		return nil
	}
}
