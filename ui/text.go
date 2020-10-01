package ui

import (
	"io"
	"os"

	"github.com/logrusorgru/aurora"
)

func Color(w io.Writer) aurora.Aurora {
	return aurora.NewAurora(true)
}

func Bold(text string) string {
	color := Color(os.Stdout)
	return color.Sprintf(color.Bold(text))
}
