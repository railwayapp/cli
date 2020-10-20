package gateway

import (
	"github.com/pkg/browser"
)

func (g *Gateway) OpenInBrowser(url string) error {
	err := browser.OpenURL(url)
	if err != nil {
		return err
	}
	return nil

}
