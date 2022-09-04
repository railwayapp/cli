package ui_test

import (
	"github.com/railwayapp/cli/ui"
	"github.com/stretchr/testify/require"
	"strings"
	"testing"
)

var keyValuesTest = []struct {
	name string
	in   map[string]string
	out  string
}{
	{
		name: "Nil map should print nothing",
		in:   nil,
		out:  "",
	},
	{
		name: "Empty map should print nothing",
		in:   map[string]string{},
		out:  "",
	},
	{
		name: "Output should always be alphabetical",
		in: map[string]string{
			"zzz": "ZZZ",
			"BBB": "bbb",
			"AAA": "aaa",
			"aaa": "AAA",
		},
		out: multiline(
			"AAA: aaa",
			"BBB: bbb",
			"aaa: AAA",
			"zzz: ZZZ",
		),
	},
	{
		name: "Varying lengths should align values",
		in: map[string]string{
			"A":   "aaa",
			"BB":  "bbbbbbbb",
			"CCC": "b",
		},
		out: multiline(
			"A:   aaa",
			"BB:  bbbbbbbb",
			"CCC: b",
		),
	},
	{
		name: "Super long keys should only pad others so much",
		in: map[string]string{
			"A": "aaa",
			"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB": "bbbbbbbb",
			"CCC": "b",
		},
		out: multiline(
			"A:                                                  aaa",
			"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB: bbbbbbbb",
			"CCC:                                                b",
		),
	},
}

var unorderedListTest = []struct {
	name string
	in   []string
	out  string
}{
	{
		name: "Nil slice should print nothing",
		in:   nil,
		out:  "",
	},
	{
		name: "Empty slice should print nothing",
		in:   []string{},
		out:  "",
	},
	{
		name: "Generic output should work",
		in: []string{
			"Foo",
			"Bar",
			"Baz",
		},
		out: multiline(
			"- Foo",
			"- Bar",
			"- Baz",
		),
	},
}

var orderedListTest = []struct {
	name string
	in   []string
	out  string
}{
	{
		name: "Nil slice should print nothing",
		in:   nil,
		out:  "",
	},
	{
		name: "Empty slice should print nothing",
		in:   []string{},
		out:  "",
	},
	{
		name: "Generic output should work",
		in: []string{
			"Foo",
			"Bar",
			"Baz",
		},
		out: multiline(
			"1) Foo",
			"2) Bar",
			"3) Baz",
		),
	},
}

var truncateTest = []struct {
	name  string
	inStr string
	inLen int
	out   string
}{
	{
		name:  "Negative numbers work",
		inStr: "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
		inLen: -10,
		out:   "0...Z",
	},
	{
		name:  "Small length works",
		inStr: "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
		inLen: 1,
		out:   "0...Z",
	},
	{
		name:  "Odd length works",
		inStr: "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
		inLen: 10,
		out:   "0123...XYZ",
	},
	{
		name:  "Even length works",
		inStr: "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
		inLen: 11,
		out:   "0123...WXYZ",
	},
	{
		name:  "Full length works",
		inStr: "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
		inLen: 100,
		out:   "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
	},
}

var paragraphTest = []struct {
	name string
	in   string
	out  string
}{
	{
		name: "Should wrap long lines",
		in:   "This is a long sentence, which is really boring, and demonstrates the wrapping functionality nicely. It should be about three lines.",
		out:  "This is a long sentence, which is really boring, and\ndemonstrates the wrapping functionality nicely. It should be\nabout three lines.\n",
	},
	{
		name: "Should wrap precisely",
		in:   "a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a",
		out:  "a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a\na a a a a a a a a a a a a a a a a a a a a a a a a a a a a a\na a a a a a a a a a a a a a a a a a a a a a a a a a a a\n",
	},
}

var prefixTest = []struct {
	name     string
	inStr    string
	inPrefix string
	out      string
}{
	{
		name:     "Should prefix lines",
		inStr:    "Line 1\nLine 2\nLine 3",
		inPrefix: "> ",
		out: multiline(
			"> Line 1",
			"> Line 2",
			"> Line 3",
		),
	},
}

func TestKeyValues(t *testing.T) {
	for _, tt := range keyValuesTest {
		t.Run(tt.name, func(t *testing.T) {
			require.Equal(t, tt.out, ui.KeyValues(tt.in))
		})
	}
}

func TestUnorderedList(t *testing.T) {
	for _, tt := range unorderedListTest {
		t.Run(tt.name, func(t *testing.T) {
			require.Equal(t, tt.out, ui.UnorderedList(tt.in))
		})
	}
}

func TestOrderedList(t *testing.T) {
	for _, tt := range orderedListTest {
		t.Run(tt.name, func(t *testing.T) {
			require.Equal(t, tt.out, ui.OrderedList(tt.in))
		})
	}
}

func TestTruncate(t *testing.T) {
	for _, tt := range truncateTest {
		t.Run(tt.name, func(t *testing.T) {
			require.Equal(t, tt.out, ui.Truncate(tt.inStr, tt.inLen))
		})
	}
}

func TestParagraph(t *testing.T) {
	for _, tt := range paragraphTest {
		t.Run(tt.name, func(t *testing.T) {
			require.Equal(t, tt.out, ui.Paragraph(tt.in))
		})
	}
}

func TestPrefixLines(t *testing.T) {
	for _, tt := range prefixTest {
		t.Run(tt.name, func(t *testing.T) {
			require.Equal(t, tt.out, ui.PrefixLines(tt.inStr, tt.inPrefix))
		})
	}
}

// multiline provides a human-readable way to create a multiline block of text
func multiline(lines ...string) string {
	return strings.Join(lines, "\n") + "\n"
}
