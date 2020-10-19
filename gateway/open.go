package gateway

import (
	"fmt"
	"strings"

	"github.com/pkg/browser"
)

func (g *Gateway) OpenInBrowser(projectId string, url string) error {
	if strings.Contains(url, "%s") {
		err := browser.OpenURL(fmt.Sprintf(url, projectId))
		if err != nil {
			return err
		}
	}
	err := browser.OpenURL(url)
	if err != nil {
		return err
	}
	return nil

}
