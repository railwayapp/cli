package main

import (
	"context"
	"fmt"
	"os"
	"strings"

	"github.com/railwayapp/cli/cmd"
	"github.com/railwayapp/cli/constants"
	"github.com/railwayapp/cli/entity"
	"github.com/spf13/cobra"
)

var rootCmd = &cobra.Command{
	Use:           "railway",
	SilenceUsage:  true,
	SilenceErrors: true,
	Version:       constants.Version,
	Short:         "🚅 Railway. Infrastructure, Instantly.",
	Long:          "Interact with 🚅 Railway via CLI \n\n Deploy infrastructure, instantly. Docs: https://railway.app/docs",
}

/* contextualize converts a HandlerFunction to a cobra function
 */
func contextualize(fn entity.HandlerFunction) entity.CobraFunction {
	return func(cmd *cobra.Command, args []string) error {
		ctx := context.Background()
		req := &entity.CommandRequest{
			Cmd:  cmd,
			Args: args,
		}
		err := fn(ctx, req)
		if err != nil {
			// TODO: Make it *pretty*
			fmt.Println(err.Error())
		}
		return nil
	}
}

func init() {
	// Initializes all commands
	handler := cmd.New()
	rootCmd.AddCommand(&cobra.Command{
		Use:   "login",
		Short: "Login to Railway",
		RunE:  contextualize(handler.Login, handler.Telemetry),
	})
	rootCmd.AddCommand(&cobra.Command{
		Use:   "logout",
		Short: "Logout of Railway",
		RunE:  contextualize(handler.Logout, handler.Telemetry),
	})
	rootCmd.AddCommand(&cobra.Command{
		Use:   "whoami",
		Short: "Show the currently logged in user",
		RunE:  contextualize(handler.Whoami, handler.Telemetry),
	})
	rootCmd.AddCommand(&cobra.Command{
		Use:               "init",
		Short:             "Initialize Railway",
		PersistentPreRunE: contextualize(handler.CheckVersion),
		RunE:              contextualize(handler.Init, handler.Telemetry),
	})
	rootCmd.AddCommand(&cobra.Command{
		Use:   "env",
		Short: "Show environment variables",
		RunE:  contextualize(handler.Env, handler.Telemetry),
	})
	rootCmd.AddCommand(&cobra.Command{
		Use:   "status",
		Short: "Show status",
		RunE:  contextualize(handler.Status, handler.Telemetry),
	})
	rootCmd.AddCommand(&cobra.Command{
		Use:   "environment",
		Short: "Select an environment",
		RunE:  contextualize(handler.Environment, handler.Telemetry),
	})
	rootCmd.AddCommand(&cobra.Command{
		Use:   "open",
		Short: "Open the project in railway",
		RunE:  contextualize(handler.Open, handler.Telemetry),
	})
	rootCmd.AddCommand(&cobra.Command{
		Use:   "list",
		Short: "Show all your projects",
		RunE:  contextualize(handler.List, handler.Telemetry),
	})
	rootCmd.AddCommand(&cobra.Command{
		Use:               "run",
		Short:             "Run command inside the Railway environment",
		PersistentPreRunE: contextualize(handler.CheckVersion),
		RunE:              contextualize(handler.Run, handler.Telemetry),
	})
	rootCmd.AddCommand(&cobra.Command{
		Use:               "version",
		Short:             "Get version of the Railway CLI",
		PersistentPreRunE: contextualize(handler.CheckVersion),
		RunE:              contextualize(handler.Version, handler.Telemetry),
	})
	rootCmd.AddCommand(&cobra.Command{
		Use:   "up",
		Short: "Upload and deploy",
		RunE:  contextualize(handler.Up, handler.Telemetry),
	})
}

func main() {
	if err := rootCmd.Execute(); err != nil {
		if strings.Contains(err.Error(), "unknown command") {
			suggStr := "\nS"

			suggestions := rootCmd.SuggestionsFor(os.Args[1])
			if len(suggestions) > 0 {
				suggStr = fmt.Sprintf(" Did you mean \"%s\"?\nIf not, s", suggestions[0])
			}

			fmt.Println(fmt.Sprintf("Unknown command \"%s\" for \"%s\".%s"+
				"ee \"railway --help\" for available commands.",
				os.Args[1], rootCmd.CommandPath(), suggStr))
		} else {
			fmt.Println(err)
		}
		os.Exit(1)
	}
}
