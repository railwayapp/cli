package cmd

import (
	"context"
	"fmt"
	"strings"

	"github.com/railwayapp/cli/constants"
	"github.com/railwayapp/cli/entity"
	"github.com/stripe/stripe-cli/pkg/open"
)

func (h *Handler) Open(ctx context.Context, req *entity.CommandRequest) error {
	projectCfg, err := h.cfg.GetProjectConfigs()
	if err != nil {
		return err
	}
	project, err := h.ctrl.GetProject(ctx, projectCfg.Project)
	if len(req.Args) == 0 {
		fmt.Println("Use railway open to open links to Railway from the CLI. Here's whats we have:")
		err = nameList(project.Id)
		if err != nil {
			return err
		}
		return nil
	}
	if url, ok := constants.DocsURLMap[req.Args[0]]; ok {
		if strings.Contains(url, "%s") {
			err = open.Browser(fmt.Sprintf(url, project.Id))
		} else {
			err = open.Browser(url)
		}
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
