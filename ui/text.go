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

func RedText(payload string) aurora.Value {
	color := Color(os.Stdout)
	return color.Red(payload)
}

func MagentaText(payload string) aurora.Value {
	color := Color(os.Stdout)
	return color.Magenta(payload)
}

func GreenText(payload string) aurora.Value {
	color := Color(os.Stdout)
	return color.Green(payload)
}

func YellowText(payload string) aurora.Value {
	color := Color(os.Stdout)
	return color.Yellow(payload)
}
