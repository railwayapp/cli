package ui

import (
	"os"
	"time"

	"github.com/briandowns/spinner"
)

type TrainSpinner []string

var (
	TrainRight  TrainSpinner = []string{"🚅", "🚅🚋", "🚅🚋🚋", "🚅🚋🚋🚋", "🚅🚋🚋🚋🚋", "🚅🚋🚋🚋🚋🚋", "🚅🚋🚋🚋🚋🚋🚋", "🚅🚋🚋🚋🚋🚋🚋🚋", "🚅🚋🚋🚋🚋🚋🚋🚋🚋"}
	TrainLeft   TrainSpinner = []string{"       🚅", "      🚅🚋", "     🚅🚋🚋", "    🚅🚋🚋🚋", "   🚅🚋🚋🚋🚋", "  🚅🚋🚋🚋🚋🚋", " 🚅🚋🚋🚋🚋🚋🚋", " 🚅🚋🚋🚋🚋🚋🚋🚋", "🚅🚋🚋🚋🚋🚋🚋🚋🚋"}
	TrainEmojis TrainSpinner = []string{"🚝", "🚅", "🚄", "🚇", "🚞", "🚈", "🚉", "🚂", "🚃", "🚊", "🚋"}
)

type SpinnerCfg struct {
	// Message specifies the text label that appears while loading
	Message string
	// Tokens is a list of emoji to rotate through, during loading
	Tokens []string
	// Duration is the amount of delay between each spinner "frame"
	Duration time.Duration
}

var s = &spinner.Spinner{}

func StartSpinner(cfg *SpinnerCfg) {
	if cfg.Tokens == nil {
		cfg.Tokens = TrainEmojis
	}
	if cfg.Duration.Microseconds() == 0 {
		cfg.Duration = time.Duration(100) * time.Millisecond
	}
	s = spinner.New(cfg.Tokens, cfg.Duration)
	s.Writer = os.Stdout

	if cfg.Message != "" {
		s.Suffix = " " + cfg.Message
	}

	s.Start()
}

func StopSpinner(msg string) {
	if msg != "" {
		s.FinalMSG = msg + "\n"
	}

	s.Stop()
}
