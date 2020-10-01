package entity

import "github.com/spf13/cobra"

type CommandRequest struct {
	Cmd  *cobra.Command
	Args []string
}

type CobraFunction func(cmd *cobra.Command, args []string) error
