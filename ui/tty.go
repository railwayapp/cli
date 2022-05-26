package ui

import (
	"github.com/mattn/go-isatty"
	"os"
)

func SupportsANSICodes() bool {
	return isatty.IsTerminal(os.Stdout.Fd())
}
