package main

import (
	"context"
	"fmt"
	"os"
	"runtime"
	"runtime/debug"
	"strings"

	"github.com/railwayapp/cli/cmd"
	"github.com/railwayapp/cli/constants"
	"github.com/railwayapp/cli/entity"
	"github.com/railwayapp/cli/ui"
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

func addRootCmd(cmd *cobra.Command) *cobra.Command {
	rootCmd.AddCommand(cmd)
	return cmd
}

// contextualize converts a HandlerFunction to a cobra function
func contextualize(fn entity.HandlerFunction, panicFn entity.PanicFunction) entity.CobraFunction {
	return func(cmd *cobra.Command, args []string) error {
		ctx := context.Background()
		defer func() {
			if r := recover(); r != nil {
				err := panicFn(ctx, fmt.Sprint(r), string(debug.Stack()), cmd.Name(), args)
				if err != nil {
					fmt.Println("Unable to relay panic to server. Are you connected to the internet?")
				}
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

	loginCmd := addRootCmd(&cobra.Command{
		Use:   "login",
		Short: "Login to your Railway account",
		RunE:  contextualize(handler.Login, handler.Panic),
	})
	loginCmd.Flags().Bool("browserless", false, "--browserless")

	addRootCmd(&cobra.Command{
		Use:   "logout",
		Short: "Logout of your Railway account",
		RunE:  contextualize(handler.Logout, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:   "whoami",
		Short: "Get the current logged in user",
		RunE:  contextualize(handler.Whoami, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:               "init",
		Short:             "Create a new Railway project",
		PersistentPreRunE: contextualize(handler.CheckVersion, handler.Panic),
		RunE:              contextualize(handler.Init, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:               "link",
		Short:             "Associate existing project with current directory",
		PersistentPreRunE: contextualize(handler.CheckVersion, handler.Panic),
		RunE:              contextualize(handler.Link, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:   "unlink",
		Short: "Disassociate project from current directory",
		RunE:  contextualize(handler.Unlink, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:        "disconnect",
		RunE:       contextualize(handler.Unlink, handler.Panic),
		Deprecated: "Please use 'railway unlink' instead", /**/
	})

	addRootCmd(&cobra.Command{
		Use:        "env",
		RunE:       contextualize(handler.Variables, handler.Panic),
		Deprecated: "Please use 'railway variables' instead", /**/
	})

	variablesCmd := addRootCmd(&cobra.Command{
		Use:     "variables",
		Aliases: []string{"vars"},
		Short:   "Show variables for active environment",
		RunE:    contextualize(handler.Variables, handler.Panic),
	})
	variablesCmd.AddCommand(&cobra.Command{
		Use:   "get",
		Short: "Get the value of a variable",
		RunE:  contextualize(handler.VariablesGet, handler.Panic),
		Args:  cobra.MinimumNArgs(1),
	})
	variablesCmd.AddCommand(&cobra.Command{
		Use:   "set",
		Short: "Create or update the value of a variable",
		RunE:  contextualize(handler.VariablesSet, handler.Panic),
		Args:  cobra.MinimumNArgs(1),
	})
	variablesCmd.AddCommand(&cobra.Command{
		Use:   "delete",
		Short: "Delete a variable",
		RunE:  contextualize(handler.VariablesDelete, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:   "status",
		Short: "Show information about the current project",
		RunE:  contextualize(handler.Status, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:   "environment",
		Short: "Change the active environment",
		RunE:  contextualize(handler.Environment, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:   "open",
		Short: "Open project dashboard in default browser",
		RunE:  contextualize(handler.Open, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:   "list",
		Short: "List all projects in your Railway account",
		RunE:  contextualize(handler.List, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:                "run",
		Short:              "Run a local command using variables from the active environment",
		PersistentPreRunE:  contextualize(handler.CheckVersion, handler.Panic),
		RunE:               contextualize(handler.Run, handler.Panic),
		DisableFlagParsing: true,
	})

	addRootCmd(&cobra.Command{
		Use:   "protect",
		Short: "[EXPERIMENTAL!] Protect current branch (Actions will require confirmation)",
		RunE:  contextualize(handler.Protect, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:               "version",
		Short:             "Get the version of the Railway CLI",
		PersistentPreRunE: contextualize(handler.CheckVersion, handler.Panic),
		RunE:              contextualize(handler.Version, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:   "up",
		Short: "Upload and deploy project from the current directory",
		RunE:  contextualize(handler.Up, handler.Panic),
	}).Flags().BoolP("detach", "d", false, "Detach from cloud build/deploy logs")

	addRootCmd(&cobra.Command{
		Use:   "logs",
		Short: "View the most-recent deploy's logs",
		RunE:  contextualize(handler.Logs, handler.Panic),
	}).Flags().Int32P("lines", "n", 0, "Output a specific number of lines")

	addRootCmd(&cobra.Command{
		Use:   "docs",
		Short: "Open Railway Documentation in default browser",
		RunE:  contextualize(handler.Docs, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:   "add",
		Short: "Add a new plugin to your project",
		RunE:  contextualize(handler.Add, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:   "connect",
		Short: "Open an interactive shell to a database",
		RunE:  contextualize(handler.Connect, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Hidden: true,
		Use:    "design",
		Short:  "Print CLI design components",
		RunE:   contextualize(handler.Design, handler.Panic),
	})
}

func main() {
	if _, err := os.Stat("/proc/version"); !os.IsNotExist(err) && runtime.GOOS == "windows" {
		fmt.Printf("%s : Running in Non standard shell!\n Please consider using something like WSL!\n", ui.YellowText(ui.Bold("[WARNING!]").String()).String())
	}
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
