package ui

import (
	"errors"
	"fmt"
	"reflect"
	"sort"
	"strings"

	"github.com/manifoldco/promptui"
	"github.com/railwayapp/cli/entity"
)

type Prompt string
type Selection string

const (
	InitNew          Selection = "Empty Project"
	InitFromTemplate Selection = "Starter Template"
)

func PromptInit() (Selection, error) {
	_, selection, err := selectString("Starting Point", []string{string(InitNew), string(InitFromTemplate)})
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
	teams := make([]string, 0)

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
		teams := make([]string, 0)
		teamCheck := make(map[string]bool)
		for _, project := range projects {
			if project.Team == nil {
				continue
			}

			// Ensure teams are only appended once by checking teamCheck
			if _, hasSeenTeam := teamCheck[*project.Team]; !hasSeenTeam {
				teams = append(teams, *project.Team)
				teamCheck[*project.Team] = true
			}
		}

		_, team, err := selectString("Team", teams)
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

	sort.Slice(filteredProjects, func(i int, j int) bool {
		return filteredProjects[i].UpdatedAt > filteredProjects[j].UpdatedAt
	})

	i, _, err := selectCustom("Project", filteredProjects, func(index int) string {
		return filteredProjects[index].Name
	})
	return filteredProjects[i], err
}

// PromptStarterTemplates prompts the user to select one of the provided starter templates
func PromptStarterTemplates(starters []*entity.Starter) (*entity.Starter, error) {
	i, _, err := selectCustom("Starter", starters, func(index int) string {
		return starters[index].Title
	})

	return starters[i], err
}

func PromptIsRepoPrivate() (bool, error) {
	_, visibility, err := selectString("Visibility", []string{"Public", "Private"})
	return visibility == "Private", err
}

func PromptEnvVars(envVars []*entity.StarterEnvVar) (map[string]string, error) {
	variables := make(map[string]string)
	if len(envVars) > 0 {
		fmt.Printf("\n%s\n", Bold("Environment Variables"))
	}

	for _, envVar := range envVars {
		prompt := promptui.Prompt{
			Label:   envVar.Name,
			Default: envVar.Default,
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

func PromptConfirmProjectName() (string, error) {
	prompt := promptui.Prompt{
		Label: "Confirm project name",
		Templates: &promptui.PromptTemplates{
			Prompt:  "{{ . }} ",
			Valid:   fmt.Sprintf("%s {{ . | bold }}: ", promptui.IconGood),
			Invalid: fmt.Sprintf("%s {{ . | bold }}: ", promptui.IconBad),
			Success: fmt.Sprintf("%s {{ . | magenta | bold }}: ", promptui.IconGood),
		},
	}
	return prompt.Run()
}

// PromptGitHubScopes prompts the user to select one of the provides scopes
func PromptGitHubScopes(scopes []string) (string, error) {
	if len(scopes) == 1 {
		return scopes[0], nil
	}

	_, scope, err := selectString("GitHub Owner", scopes)
	return scope, err
}

func PromptEnvironments(environments []*entity.Environment) (*entity.Environment, error) {
	if len(environments) == 1 {
		environment := environments[0]
		fmt.Printf("%s Environment: %s\n", promptui.IconGood, BlueText(environment.Name))
		return environment, nil
	}
	i, _, err := selectCustom("Environment", environments, func(index int) string {
		return environments[index].Name
	})
	if err != nil {
		return nil, err
	}

	return environments[i], nil
}

func PromptPlugins(plugins []string) (string, error) {
	i, _, err := selectString("Plugin", plugins)
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

// selectWrapper wraps an arbitrary stringify function + associated index, used by the select
// helpers so it can accept an arbitrary slice. It also implements the Stringer interface so
// it can automatically be printed by %s
type selectItemWrapper struct {
	stringify func(index int) string
	index     int
}

// String adheres to the Stringer interface and returns the string representation from the
// stringify function
func (w selectItemWrapper) String() string {
	return w.stringify(w.index)
}

// selectString prompts the user to select a string from the provided slice
func selectString(label string, items []string) (int, string, error) {
	return selectCustom(label, items, func(index int) string {
		return fmt.Sprintf("%v", items[index])
	})
}

// selectCustom prompts the user to select an item from the provided slice. A stringify function is passed, which
// is responsible for returning a label for the item, when called.
func selectCustom(label string, items interface{}, stringify func(index int) string) (int, string, error) {
	v := reflect.ValueOf(items)
	if v.Kind() != reflect.Slice {
		panic(fmt.Errorf("forEachValue: expected slice type, found %q", v.Kind().String()))
	}
	wrappedItems := make([]selectItemWrapper, 0)
	for i := 0; i < v.Len(); i++ {
		wrappedItems = append(wrappedItems, selectItemWrapper{
			stringify: stringify,
			index:     i,
		})
	}

	options := &promptui.Select{
		Label: fmt.Sprintf("Select %s", label),
		Items: wrappedItems,
		Size:  10,
		Templates: &promptui.SelectTemplates{
			Active:   fmt.Sprintf(`%s {{ . | underline }}`, promptui.IconSelect),
			Inactive: `  {{ . }}`,
			Selected: fmt.Sprintf("%s %s: {{ . | magenta | bold }} ", promptui.IconGood, label),
		},
		Searcher: func(input string, i int) bool {
			return strings.Contains(
				strings.ToLower(stringify(i)),
				strings.ToLower(input),
			)
		},
	}

	return options.Run()
}
