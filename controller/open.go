package controller

import (
	"context"
	"fmt"
	"strings"

	"github.com/railwayapp/cli/constants"
)

// OpenInBrowser opens the provided url in the browser
func (c *Controller) OpenInBrowser(ctx context.Context, args []string, projectId string) error {
	if len(args) == 0 {
		nameList(projectId)
	}
	return nil
}

func nameList(id string) error {
	names, longest := getNames()
	fmt.Printf("%s%s\n", padName("shortcut", longest), "    url")
	fmt.Printf("%s%s\n", padName("--------", longest), "    ---------")

	for _, names := range names {
		url := constants.DocsURLMap[names]
		if strings.Contains(url, "%s") {
			url = fmt.Sprintf(url, id)
		}
		paddedName := padName(names, longest)
		fmt.Printf("%s => %s\n", paddedName, url)
	}
	return nil
}

func padName(name string, length int) string {
	difference := length - len(name)

	var b strings.Builder

	fmt.Fprint(&b, name)

	for i := 0; i < difference; i++ {
		fmt.Fprint(&b, " ")
	}

	return b.String()
}

func getNames() ([]string, int) {
	longest := 0
	keys := make([]string, 0, len(constants.DocsURLMap))
	for k := range constants.DocsURLMap {
		if len(k) > longest {
			longest = len(k)
		}
		keys = append(keys, k)
	}
	return keys, longest
}
