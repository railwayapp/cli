package main

import (
	"context"
	"fmt"
	"os"
	"runtime/debug"
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
	Long:          "Interact with 🚅 Railway via CLI \n\n Deploy infrastructure, instantly. Docs: https://docs.railway.app",
}

/* contextualize converts a HandlerFunction to a cobra function
 */
func contextualize(fn entity.HandlerFunction, panicFn entity.PanicFunction) entity.CobraFunction {
	return func(cmd *cobra.Command, args []string) error {
		ctx := context.Background()
		defer func() {
			if r := recover(); r != nil {
				panicFn(ctx, fmt.Sprint(r), string(debug.Stack()), cmd.Name(), args)
			}
		}()

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

	loginCmd := &cobra.Command{
		Use:   "login",
		Short: "Login to Railway",
		RunE:  contextualize(handler.Login, handler.Panic),
	}
	loginCmd.Flags().Bool("browserless", false, "--browserless")

	logoutCmd := &cobra.Command{
		Use:   "logout",
		Short: "Logout of Railway",
		RunE:  contextualize(handler.Logout, handler.Panic),
	}

	whoamiCmd := &cobra.Command{
		Use:   "whoami",
		Short: "Show the currently logged in user",
		RunE:  contextualize(handler.Whoami, handler.Panic),
	}

	initCmd := &cobra.Command{
		Use:               "init",
		Short:             "Initialize Railway",
		PersistentPreRunE: contextualize(handler.CheckVersion, handler.Panic),
		RunE:              contextualize(handler.Init, handler.Panic),
	}

	disconnectCmd := &cobra.Command{
		Use:   "disconnect",
		Short: "Disconnect from Railway",
		RunE:  contextualize(handler.Disconnect, handler.Panic),
	}

	envCmd := &cobra.Command{
		Use:   "env",
		Short: "Show environment variables",
		RunE:  contextualize(handler.Env, handler.Panic),
	}
	envCmd.AddCommand(&cobra.Command{
		Use:   "set",
		Short: "Add or change value of variable",
		RunE:  contextualize(handler.EnvSet, handler.Panic),
	})
	envCmd.AddCommand(&cobra.Command{
		Use:   "get",
		Short: "get the value of a variable",
		RunE:  contextualize(handler.EnvGet, handler.Panic),
	})
	envCmd.AddCommand(&cobra.Command{
		Use:   "delete",
		Short: "Delete a variable",
		RunE:  contextualize(handler.EnvDelete, handler.Panic),
	})

	statusCmd := &cobra.Command{
		Use:   "status",
		Short: "Show status",
		RunE:  contextualize(handler.Status, handler.Panic),
	}

	environmentCmd := &cobra.Command{
		Use:   "environment",
		Short: "Select an environment",
		RunE:  contextualize(handler.Environment, handler.Panic),
	}

	openCmd := &cobra.Command{
		Use:   "open",
		Short: "Open the project in railway",
		RunE:  contextualize(handler.Open, handler.Panic),
	}

	listCmd := &cobra.Command{
		Use:   "list",
		Short: "Show all your projects",
		RunE:  contextualize(handler.List, handler.Panic),
	}

	runCmd := &cobra.Command{
		Use:                "run",
		Short:              "Run command inside the Railway environment",
		PersistentPreRunE:  contextualize(handler.CheckVersion, handler.Panic),
		RunE:               contextualize(handler.Run, handler.Panic),
		DisableFlagParsing: true,
	}

	protectCmd := &cobra.Command{
		Use:   "protect",
		Short: "[EXPERIMENTAL!] Protect current branch (Actions will require confirmation)",
		RunE:  contextualize(handler.Protect, handler.Panic),
	}

	versionCmd := &cobra.Command{
		Use:               "version",
		Short:             "Get version of the Railway CLI",
		PersistentPreRunE: contextualize(handler.CheckVersion, handler.Panic),
		RunE:              contextualize(handler.Version, handler.Panic),
	}

	upCmd := &cobra.Command{
		Use:   "up",
		Short: "Upload and deploy",
		RunE:  contextualize(handler.Up, handler.Panic),
	}

	docsCmd := &cobra.Command{
		Use:   "docs",
		Short: "Open Railway Docs in browser",
		RunE:  contextualize(handler.Docs, handler.Panic),
	}

	addCmd := &cobra.Command{
		Use:   "add",
		Short: "Add Railway plugins",
		RunE:  contextualize(handler.Add, handler.Panic),
	}

	connectCmd := &cobra.Command{
		Use:   "connect",
		Short: "Connect to your Railway database",
		RunE:  contextualize(handler.Connect, handler.Panic),
	}

	rootCmd.AddCommand(
		loginCmd,
		logoutCmd,
		whoamiCmd,
		initCmd,
		disconnectCmd,
		envCmd,
		statusCmd,
		environmentCmd,
		openCmd,
		listCmd,
		runCmd,
		protectCmd,
		versionCmd,
		upCmd,
		docsCmd,
		addCmd,
		connectCmd,
	)
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
