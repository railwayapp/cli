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
	return aurora.Red(payload)
}

func MagentaText(payload string) aurora.Value {
	return aurora.Magenta(payload)

}

func GreenText(payload string) aurora.Value {
	return aurora.Green(payload)
}

func YellowText(payload string) aurora.Value {
	return aurora.Yellow(payload)
}
