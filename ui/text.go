package ui

import (
	"fmt"
	_aurora "github.com/logrusorgru/aurora"
	"math"
	"sort"
	"strings"
)

var aurora = _aurora.NewAurora(true)

func DisableTextStyles() {
	aurora = _aurora.NewAurora(false)
}

func Bold(payload string) _aurora.Value {
	return aurora.Bold(payload)
}

func RedText(payload string) _aurora.Value {
	return aurora.Red(payload)
}

func MagentaText(payload string) _aurora.Value {
	return aurora.Magenta(payload)
}

func BlueText(payload string) _aurora.Value {
	return aurora.Blue(payload)
}

func GrayText(payload string) _aurora.Value {
	return aurora.Gray(10, payload)
}

func LightGrayText(payload string) _aurora.Value {
	return aurora.Gray(14, payload)
}

func GreenText(payload string) _aurora.Value {
	return aurora.Green(payload)
}

func YellowText(payload string) _aurora.Value {
	return aurora.Yellow(payload)
}

func Heading(text string) string {
	return _aurora.Sprintf(_aurora.Bold("==> %s\n").Magenta(), Bold(text))
}

func AlertDanger(text string) string {
	return _aurora.Sprintf(_aurora.Bold("🚨 %s\n").Red(), text)
}

func AlertWarning(text string) string {
	return _aurora.Sprintf(_aurora.Bold("⚠️ %s\n").Yellow(), text)
}

func AlertInfo(text string) string {
	return _aurora.Sprintf(GrayText(Bold("💁 %s\n").String()), text)
}

func Truncate(text string, maxLength int) string {
	ellipsis := "..."
	overflow := len(text) + len(ellipsis) - maxLength
	if overflow < 0 {
		return text
	}

	visibleSize := float64(len(text)-overflow) / 2
	if visibleSize < 1 {
		visibleSize = 1
	}

	// Account for visibleSize not being whole number
	prefixLen := int(math.Ceil(visibleSize))
	suffixLen := int(math.Floor(visibleSize))

	prefix := text[:prefixLen]
	suffix := text[len(text)-suffixLen:]

	return prefix + ellipsis + suffix
}

func ObscureText(text string) string {
	return strings.Repeat("*", len(text))
}

func UnorderedList(items []string) string {
	text := ""
	for _, item := range items {
		text += fmt.Sprintf("%s %s\n", Bold(LightGrayText("-").String()), item)
	}
	return text
}

func OrderedList(items []string) string {
	text := ""
	for i, item := range items {
		index := Bold(LightGrayText(fmt.Sprintf("%d)", i+1)).String())
		text += fmt.Sprintf("%s %s\n", index, item)
	}
	return text
}

// Indent adds two space characters to the start of every line in the text
func Indent(text string) string {
	return PrefixLines(text, "  ")
}

// Paragraph automatically wraps text (by word) to 60 chars
func Paragraph(text string) string {
	const maxLineLength = 60

	lines := make([]string, 0)
	currentLine := ""
	for _, word := range strings.Split(text, " ") {
		currentLineWithNextWord := currentLine
		if currentLineWithNextWord == "" {
			currentLineWithNextWord += word
		} else {
			currentLineWithNextWord += " " + word
		}

		if len(currentLineWithNextWord) > maxLineLength {
			lines = append(lines, currentLine)
			currentLine = word
		} else {
			currentLine = currentLineWithNextWord
		}
	}

	// Add unfinished line if there was one
	if currentLine != "" {
		lines = append(lines, currentLine)
	}

	return strings.Join(lines, "\n") + "\n"
}

// BlockQuote adds two space characters to the start of every line in the text
func BlockQuote(text string) string {
	wrapped := strings.TrimSpace(Paragraph(text))
	return PrefixLines(wrapped, GrayText("> ").String())
}

// PrefixLines adds a string to the start of every line in the text
func PrefixLines(text, prefix string) string {
	newText := ""
	for _, line := range strings.Split(text, "\n") {
		newText += fmt.Sprintf("%s%s\n", prefix, line)
	}
	return newText
}

func KeyValues(items map[string]string) string {
	type pair struct {
		Key   string
		Value string
	}

	// Need to move them into slice because maps have random order
	pairs := make([]pair, 0)

	var maxKeyLengthWithPadding = 50
	var longestKey = 0

	// Add pairs to slice and find longest key
	for k, v := range items {
		pairs = append(pairs, pair{Key: k, Value: v})
		if len(k) > longestKey {
			longestKey = len(k)
		}
	}

	text := ""

	// order the pairs by key
	sort.Slice(pairs, func(i, j int) bool {
		return strings.Compare(pairs[j].Key, pairs[i].Key) > 0
	})

	nameLength := min(longestKey, maxKeyLengthWithPadding)
	for _, pair := range pairs {
		prettyKey := fmt.Sprintf("%s:", GreenText(pair.Key))
		padding := strings.Repeat(" ", max(0, nameLength-len(pair.Key)))
		text += fmt.Sprintf("%s%s %s\n", prettyKey, padding, pair.Value)
	}

	return text
}

func max(x, y int) int {
	if x > y {
		return x
	}
	return y
}

func min(x, y int) int {
	if x < y {
		return x
	}
	return y
}
