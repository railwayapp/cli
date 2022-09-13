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
			// Skip recover during development, so we can see the panic stack traces instead of going
			// through the "send to Railway" flow and hiding the stack from the user
			if constants.IsDevVersion() {
				return
			}

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
			fmt.Println(ui.AlertDanger(err.Error()))
			os.Exit(1) // Set non-success exit code on error
		}
		return nil
	}
}

func init() {
	// Initializes all commands
	handler := cmd.New()

	rootCmd.PersistentFlags().BoolP("verbose", "v", false, "Print verbose output")

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
		Short:             "Associate existing project with current directory, may specify projectId as an argument",
		PersistentPreRunE: contextualize(handler.CheckVersion, handler.Panic),
		RunE:              contextualize(handler.Link, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:   "unlink",
		Short: "Disassociate project from current directory",
		RunE:  contextualize(handler.Unlink, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:   "delete [projectId]",
		Short: "Delete Project, may specify projectId as an argument",
		RunE:  contextualize(handler.Delete, handler.Panic),
		Args:  cobra.MinimumNArgs(1),
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
	variablesCmd.Flags().StringP("service", "s", "", "Fetch variables accessible to a specific service")

	variablesGetCmd := &cobra.Command{
		Use:     "get key",
		Short:   "Get the value of a variable",
		RunE:    contextualize(handler.VariablesGet, handler.Panic),
		Args:    cobra.MinimumNArgs(1),
		Example: "  railway variables get MY_KEY",
	}
	variablesCmd.AddCommand(variablesGetCmd)
	variablesGetCmd.Flags().StringP("service", "s", "", "Fetch variables accessible to a specific service")

	variablesSetCmd := &cobra.Command{
		Use:     "set key=value",
		Short:   "Create or update the value of a variable",
		RunE:    contextualize(handler.VariablesSet, handler.Panic),
		Args:    cobra.MinimumNArgs(1),
		Example: "  railway variables set NODE_ENV=prod NODE_VERSION=12",
	}
	variablesCmd.AddCommand(variablesSetCmd)
	variablesSetCmd.Flags().StringP("service", "s", "", "Fetch variables accessible to a specific service")
	variablesSetCmd.Flags().Bool("skip-redeploy", false, "Skip redeploying the specified service after changing the variables")
	variablesSetCmd.Flags().Bool("replace", false, "Fully replace all previous variables instead of updating them")
	variablesSetCmd.Flags().Bool("yes", false, "Skip all confirmation dialogs")

	variablesDeleteCmd := &cobra.Command{
		Use:     "delete key",
		Short:   "Delete a variable",
		RunE:    contextualize(handler.VariablesDelete, handler.Panic),
		Example: "  railway variables delete MY_KEY",
	}
	variablesCmd.AddCommand(variablesDeleteCmd)
	variablesDeleteCmd.Flags().StringP("service", "s", "", "Fetch variables accessible to a specific service")
	variablesDeleteCmd.Flags().Bool("skip-redeploy", false, "Skip redeploying the specified service after changing the variables")

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

	openCmd := addRootCmd(&cobra.Command{
		Use:   "open",
		Short: "Open your project dashboard",
		RunE:  contextualize(handler.Open, handler.Panic),
	})
	openCmd.AddCommand(&cobra.Command{
		Use:     "metrics",
		Short:   "Open project metrics",
		Aliases: []string{"m"},
		RunE:    contextualize(handler.Open, handler.Panic),
	})
	openCmd.AddCommand(&cobra.Command{
		Use:     "settings",
		Short:   "Open project settings",
		Aliases: []string{"s"},
		RunE:    contextualize(handler.Open, handler.Panic),
	})
	openCmd.AddCommand(&cobra.Command{
		Use:     "live",
		Short:   "Open the deployed application",
		Aliases: []string{"l"},
		RunE:    contextualize(handler.OpenApp, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:   "list",
		Short: "List all projects in your Railway account",
		RunE:  contextualize(handler.List, handler.Panic),
	})

	runCmd := addRootCmd(&cobra.Command{
		Use:                "run",
		Short:              "Run a local command using variables from the active environment",
		PersistentPreRunE:  contextualize(handler.CheckVersion, handler.Panic),
		RunE:               contextualize(handler.Run, handler.Panic),
		DisableFlagParsing: true,
	})
	runCmd.Flags().Bool("ephemeral", false, "Run the local command in an ephemeral environment")
	runCmd.Flags().String("service", "", "Run the command using variables from the specified service")

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

	upCmd := addRootCmd(&cobra.Command{
		Use:   "up",
		Short: "Upload and deploy project from the current directory",
		RunE:  contextualize(handler.Up, handler.Panic),
	})
	upCmd.Flags().BoolP("detach", "d", false, "Detach from cloud build/deploy logs")
	upCmd.Flags().StringP("environment", "e", "", "Specify an environment to up onto")
	upCmd.Flags().StringP("service", "s", "", "Fetch variables accessible to a specific service")

	downCmd := addRootCmd(&cobra.Command{
		Use:   "down",
		Short: "Remove the most recent deployment",
		RunE:  contextualize(handler.Down, handler.Panic),
	})

	downCmd.Flags().StringP("environment", "e", "", "Specify an environment to delete from")
	downCmd.Flags().Bool("yes", false, "Skip all confirmation dialogs")

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

	shellCmd := addRootCmd(&cobra.Command{
		Use:   "shell",
		Short: "Open a subshell with Railway variables available",
		RunE:  contextualize(handler.Shell, handler.Panic),
	})
	shellCmd.Flags().StringP("service", "s", "", "Use variables accessible to a specific service")

	addRootCmd(&cobra.Command{
		Hidden: true,
		Use:    "design",
		Short:  "Print CLI design components",
		RunE:   contextualize(handler.Design, handler.Panic),
	})

	addRootCmd(&cobra.Command{
		Use:   "completion [bash|zsh|fish|powershell]",
		Short: "Generate completion script",
		Long: `To load completions:

	Bash:

	  $ source <(railway completion bash)

	  # To load completions for each session, execute once:
	  # Linux:
	  $ railway completion bash > /etc/bash_completion.d/railway
	  # macOS:
	  $ railway completion bash > /usr/local/etc/bash_completion.d/railway

	Zsh:

	  # If shell completion is not already enabled in your environment,
	  # you will need to enable it.  You can execute the following once:

	  $ echo "autoload -U compinit; compinit" >> ~/.zshrc

	  # To load completions for each session, execute once:
	  $ railway completion zsh > "${fpath[1]}/_railway"

	  # You will need to start a new shell for this setup to take effect.

	fish:

	  $ railway completion fish | source

	  # To load completions for each session, execute once:
	  $ railway completion fish > ~/.config/fish/completions/railway.fish

	PowerShell:

	  PS> railway completion powershell | Out-String | Invoke-Expression

	  # To load completions for every new session, run:
	  PS> railway completion powershell > railway.ps1
	  # and source this file from your PowerShell profile.
	`,
		DisableFlagsInUseLine: true,
		ValidArgs:             []string{"bash", "zsh", "fish", "powershell"},
		Args:                  cobra.ExactValidArgs(1),
		RunE:                  contextualize(handler.Completion, handler.Panic),
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
