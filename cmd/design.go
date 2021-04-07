package cmd

import (
	"context"
	"fmt"

	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
)

func (h *Handler) Design(ctx context.Context, req *entity.CommandRequest) error {
	fmt.Print(ui.Heading("Alerts"))
	fmt.Print(ui.AlertDanger("Something bad is going to happen!"))
	fmt.Print(ui.AlertWarning("That might not have been what you wanted"))
	fmt.Print(ui.AlertInfo("Just so you know you know, Railway is awesome"))

	fmt.Println("")
	fmt.Print(ui.Heading("Unordered List"))
	fmt.Print(ui.UnorderedList([]string{
		"List Item 1",
		"List Item 2",
		"List Item 3",
	}))

	fmt.Println("")
	fmt.Print(ui.Heading("Ordered List"))
	fmt.Print(ui.OrderedList([]string{
		"First Step",
		"Next Step",
	}))

	fmt.Println("")
	fmt.Print(ui.Heading("Key-Value Pairs"))
	fmt.Print(ui.KeyValues(map[string]string{
		"First Key":  "Value 1",
		"Second Key": "Value 2",
		"Third Key":  "Value 3",
	}))

	fmt.Println("")
	fmt.Print(ui.Heading("Truncated Text"))
	fmt.Println(ui.Truncate("012345678901234567890123456789", 50))
	fmt.Println(ui.Truncate("012345678901234567890123456789", 10))
	fmt.Println(ui.Truncate("012345678901234567890123456789", 0))

	fmt.Println("")
	fmt.Print(ui.Heading("Secret Text"))
	fmt.Printf("My super secret password is %s, the name of my childhood pet.\n", ui.ObscureText("Luke"))

	fmt.Println("")
	fmt.Print(ui.Heading("Paragraph"))
	fmt.Print(ui.Paragraph("Paragraphs print the given text, but wrap it automatically when the lines are too long. It's super convenient!"))

	fmt.Println("")
	fmt.Print(ui.Heading("Indented"))
	fmt.Print(ui.Indent("func main() {\n  println(\"Hello World!\")\n}"))

	fmt.Println("")
	fmt.Print(ui.Heading("Block Quote"))
	fmt.Print(ui.BlockQuote("That's the thing about counter-intuitive ideas. They contradict your intuitions. So, they seem wrong"))

	fmt.Println("")
	fmt.Print(ui.Heading("Generic Line Prefix"))
	fmt.Print(ui.PrefixLines("Line 1\nLine 2\nLine 3", "🥳 "))

	return nil
}
