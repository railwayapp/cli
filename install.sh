#!/usr/bin/env bash

# Adapted from https://github.com/starship/starship/blob/master/install/install.sh

help_text="Options

   -V, --verbose
   Enable verbose output for the installer

   -f, -y, --force, --yes
   Skip the confirmation prompt during installation

   -p, --platform
   Override the platform identified by the installer

   -b, --bin-dir
   Override the bin installation directory
   Precedence: --bin-dir > RAILWAY_BIN_DIR > \$RAILWAY_HOME/bin > ~/.railway/bin

   -a, --arch
   Override the architecture identified by the installer

   -B, --base-url
   Override the base URL used for downloading releases

   --agents
   Install or reuse the Railway CLI, then configure Railway agent support

   --remote
   When used with --agents, configure the remote HTTP MCP server at
   mcp.railway.com instead of the local stdio server

   -r, --remove
   Uninstall railway

   -h, --help
   Get some help

"

set -eu
printf '\n'

BOLD="$(tput bold 2>/dev/null || printf '')"
GREY="$(tput setaf 0 2>/dev/null || printf '')"
UNDERLINE="$(tput smul 2>/dev/null || printf '')"
RED="$(tput setaf 1 2>/dev/null || printf '')"
GREEN="$(tput setaf 2 2>/dev/null || printf '')"
YELLOW="$(tput setaf 3 2>/dev/null || printf '')"
BLUE="$(tput setaf 4 2>/dev/null || printf '')"
MAGENTA="$(tput setaf 5 2>/dev/null || printf '')"
NO_COLOR="$(tput sgr0 2>/dev/null || printf '')"

SUPPORTED_TARGETS="x86_64-unknown-linux-gnu x86_64-unknown-linux-musl \
                  i686-unknown-linux-musl aarch64-unknown-linux-musl \
                  arm-unknown-linux-musleabihf x86_64-apple-darwin \
                  aarch64-apple-darwin x86_64-pc-windows-msvc \
                  i686-pc-windows-msvc aarch64-pc-windows-msvc \
                  x86_64-unknown-freebsd"

info() {
  printf '%s\n' "${BOLD}${GREY}>${NO_COLOR} $*"
}

debug() {
  if [ -n "${VERBOSE}" ]; then
    printf '%s\n' "${BOLD}${GREY}>${NO_COLOR} $*"
  fi
}

warn() {
  printf '%s\n' "${YELLOW}! $*${NO_COLOR}"
}

error() {
  printf '%s\n' "${RED}x $*${NO_COLOR}" >&2
}

completed() {
  printf '%s\n' "${GREEN}✓${NO_COLOR} $*"
}

has() {
  command -v "$1" 1>/dev/null 2>&1
}

RANDOM_FOR_SH=$(od -vAn -N4 -tu4 < /dev/urandom | sed 's/\t*$//g')

# Removes leading whitespace
RANDOM_FOR_SH=$(echo ${RANDOM_FOR_SH:-$RANDOM})

# Gets path to a temporary file, even if
get_tmpfile() {
  local suffix
  suffix="$1"
  if has mktemp; then
    printf "%s%s.%s.%s" "$(mktemp)" "-railway" "${RANDOM_FOR_SH}" "${suffix}"
  else
    # No really good options here--let's pick a default + hope
    printf "/tmp/railway.%s" "${suffix}"
  fi
}

# Test if a location is writeable by trying to write to it. Windows does not let
# you test writeability other than by writing: https://stackoverflow.com/q/1999988
test_writeable() {
  local path
  path="${1:-}/test.txt"
  if touch "${path}" 2>/dev/null; then
    rm "${path}"
    return 0
  else
    return 1
  fi
}

default_railway_home() {
  if [ -n "${RAILWAY_HOME-}" ]; then
    printf '%s' "${RAILWAY_HOME}"
    return 0
  fi

  if [ -n "${HOME-}" ]; then
    printf '%s' "${HOME}/.railway"
    return 0
  fi

  return 1
}

tildify() {
  if [ -n "${HOME-}" ]; then
    case "$1" in
      "$HOME"/*) printf '~/%s' "${1#"$HOME"/}" ;;
      "$HOME") printf '~' ;;
      *) printf '%s' "$1" ;;
    esac
  else
    printf '%s' "$1"
  fi
}

shell_quote() {
  local value="$1"
  value=${value//\'/\'\\\'\'}
  printf "'%s'" "$value"
}

fish_quote() {
  local value="$1"
  value=${value//\\/\\\\}
  value=${value//\'/\\\'}
  printf "'%s'" "$value"
}

source_path() {
  local path="$1"

  if [ -n "${HOME-}" ]; then
    case "$path" in
      "$HOME"/*) printf '"$HOME/%s"' "${path#"$HOME"/}"; return ;;
    esac
  fi

  shell_quote "$path"
}

fish_source_path() {
  local path="$1"

  if [ -n "${HOME-}" ]; then
    case "$path" in
      "$HOME"/*) printf '"$HOME/%s"' "${path#"$HOME"/}"; return ;;
    esac
  fi

  fish_quote "$path"
}

bin_dir_uses_railway_home() {
  [ -n "${RAILWAY_HOME_DIR-}" ] && [ "${BIN_DIR%/}" = "${RAILWAY_HOME_DIR%/}/bin" ]
}

download() {
  file="$1"
  url="$2"
  touch "$file"

  if has curl; then
    cmd="curl --fail --silent --location --output $file $url"
  elif has wget; then
    cmd="wget --quiet --output-document=$file $url"
  elif has fetch; then
    cmd="fetch --quiet --output=$file $url"
  else
    error "No HTTP download program (curl, wget, fetch) found, exiting…"
    return 1
  fi

  $cmd && return 0 || rc=$?

  error "Command failed (exit code $rc): ${BLUE}${cmd}${NO_COLOR}"
  printf "\n" >&2
  info "This is likely due to railway not yet supporting your configuration."
  info "If you would like to see a build for your configuration,"
  info "please create an issue requesting a build for ${MAGENTA}${TARGET}${NO_COLOR}:"
  info "${BOLD}${UNDERLINE}https://github.com/railwayapp/cli/issues/new/${NO_COLOR}"
  return $rc
}

unpack() {
  local archive=$1
  local bin_dir=$2
  local sudo=${3-}

  case "$archive" in
    *.tar.gz)
      flags=$(test -n)
      ${sudo} tar "${flags}" -xzf "${archive}" -C "${bin_dir}"
      return 0
      ;;
    *.zip)
      flags=$(test -z)
      UNZIP="${flags}" ${sudo} unzip "${archive}" -d "${bin_dir}"
      return 0
      ;;
  esac

  error "Unknown package extension."
  printf "\n"
  info "This almost certainly results from a bug in this script--please file a"
  info "bug report at https://github.com/railwayapp/cli/issues"
  return 1
}

elevate_priv() {
  if ! has sudo; then
    error 'Could not find the command "sudo", needed to get permissions for install.'
    info "If you are on Windows, please run your shell as an administrator, then"
    info "rerun this script. Otherwise, please run this script as root, or install"
    info "sudo."
    exit 1
  fi
  if ! sudo -v; then
    error "Superuser not granted, aborting installation"
    exit 1
  fi
}

install() {
  local msg
  local sudo
  local archive
  local ext="$1"

  if test_writeable "${BIN_DIR}"; then
    sudo=""
    msg="Installing railway, please wait…"
  else
    warn "Escalated permissions are required to install to ${BIN_DIR}"
    elevate_priv
    sudo="sudo"
    msg="Installing railway as root, please wait…"
  fi
  info "$msg"

  archive=$(get_tmpfile "$ext")

  # download to the temp file
  download "${archive}" "${URL}"

  # unpack the temp file to the bin dir, using sudo if required
  unpack "${archive}" "${BIN_DIR}" "${sudo}"

  # remove tempfile

  # rm "${archive}"
}

# Currently supporting:
#   - win (Git Bash)
#   - darwin
#   - linux
#   - linux_musl (Alpine)
#   - freebsd
detect_platform() {
  local platform
  platform="$(uname -s | tr '[:upper:]' '[:lower:]')"

  case "${platform}" in
    msys_nt*) platform="pc-windows-msvc" ;;
    cygwin_nt*) platform="pc-windows-msvc";;
    # mingw is Git-Bash
    mingw*) platform="pc-windows-msvc" ;;
    # use the statically compiled musl bins on linux to avoid linking issues.
    linux) platform="unknown-linux-musl" ;;
    darwin) platform="apple-darwin" ;;
    freebsd) platform="unknown-freebsd" ;;
  esac

  printf '%s' "${platform}"
}

# Currently supporting:
#   - x86_64
#   - i386
detect_arch() {
  local arch
  arch="$(uname -m | tr '[:upper:]' '[:lower:]')"

  case "${arch}" in
    amd64) arch="x86_64" ;;
    armv*) arch="arm" ;;
    arm64) arch="aarch64" ;;
  esac

  # `uname -m` in some cases mis-reports 32-bit OS as 64-bit, so double check
  if [ "${arch}" = "x86_64" ] && [ "$(getconf LONG_BIT)" -eq 32 ]; then
    arch=i686
  elif [ "${arch}" = "aarch64" ] && [ "$(getconf LONG_BIT)" -eq 32 ]; then
    arch=arm
  fi

  printf '%s' "${arch}"
}

detect_target() {
  local arch="$1"
  local platform="$2"
  local target="$arch-$platform"

  if [ "${target}" = "arm-unknown-linux-musl" ]; then
    target="${target}eabihf"
  fi

  printf '%s' "${target}"
}


confirm() {
  if [ -t 0 ]; then
    if [ -z "${FORCE-}" ]; then
      printf "%s " "${MAGENTA}?${NO_COLOR} $* ${BOLD}[y/N]${NO_COLOR}"
      set +e
      read -r yn </dev/tty
      rc=$?
      set -e
      if [ $rc -ne 0 ]; then
        error "Error reading from prompt (please re-run with the '--yes' option)"
        exit 1
      fi
      if [ "$yn" != "y" ] && [ "$yn" != "yes" ]; then
        error 'Aborting (please answer "yes" to continue)'
        exit 1
      fi
    fi
  fi
}

check_bin_dir() {
  local bin_dir="$1"

  if [ ! -d "$bin_dir" ]; then
    error "Installation location $bin_dir does not appear to be a directory"
    info "Make sure the location exists and is a directory, then try again."
    exit 1
  fi
}

is_build_available() {
  local arch="$1"
  local platform="$2"
  local target="$3"

  local good

  good=$(
    IFS=" "
    for t in $SUPPORTED_TARGETS; do
      if [ "${t}" = "${target}" ]; then
        printf 1
        break
      fi
    done
  )

  if [ "${good}" != "1" ]; then
    error "${arch} builds for ${platform} are not yet available for railway"
    printf "\n" >&2
    info "If you would like to see a build for your configuration,"
    info "please create an issue requesting a build for ${MAGENTA}${target}${NO_COLOR}:"
    info "${BOLD}${UNDERLINE}https://github.com/railwayapp/cli/issues/new/${NO_COLOR}"
    printf "\n"
    exit 1
  fi
}
UNINSTALL=0
HELP=0
AGENTS=0
REMOTE=0
RAILWAY_HOME_DIR=""
RAILWAY_ENV_FILE=""
RAILWAY_FISH_ENV_FILE=""
PATH_ACTIVATION_PRINTED=0
RAILWAY_PATH_MARKER_BEGIN="# >>> railway initialize >>>"
RAILWAY_PATH_MARKER_END="# <<< railway initialize <<<"
SHELL_STARTUP_FILE=""
SHELL_STARTUP_ACTION=""

DEFAULT_VERSION=$(curl -s https://api.github.com/repos/railwayapp/cli/releases/latest | grep -o '"tag_name":[[:space:]]*"v[^"]*"' | cut -d'"' -f4 | cut -c2-)

if [ -z "$DEFAULT_VERSION" ]; then
  error "Failed to fetch latest version from GitHub"
  exit 1
fi


# defaults
if [ -z "${RAILWAY_VERSION-}" ]; then
  RAILWAY_VERSION="$DEFAULT_VERSION"
fi

if [ -z "${RAILWAY_PLATFORM-}" ]; then
  PLATFORM="$(detect_platform)"
fi

if RAILWAY_HOME_DIR="$(default_railway_home)"; then
  if [ -n "${RAILWAY_BIN_DIR-}" ]; then
    BIN_DIR="${RAILWAY_BIN_DIR}"
  else
    BIN_DIR="${RAILWAY_HOME_DIR%/}/bin"
  fi
else
  if [ -n "${RAILWAY_BIN_DIR-}" ]; then
    BIN_DIR="${RAILWAY_BIN_DIR}"
  else
    BIN_DIR=""
  fi
fi

if [ -z "${RAILWAY_ARCH-}" ]; then
  ARCH="$(detect_arch)"
fi

if [ -z "${RAILWAY_BASE_URL-}" ]; then
  BASE_URL="https://github.com/railwayapp/cli/releases"
fi

# parse argv variables
while [ "$#" -gt 0 ]; do
  case "$1" in
  -p | --platform)
    PLATFORM="$2"
    shift 2
    ;;
  -b | --bin-dir)
    BIN_DIR="$2"
    shift 2
    ;;
  -a | --arch)
    ARCH="$2"
    shift 2
    ;;
  -B | --base-url)
    BASE_URL="$2"
    shift 2
    ;;

  -V | --verbose)
    VERBOSE=1
    shift 1
    ;;
  -f | -y | --force | --yes)
    FORCE=1
    shift 1
    ;;
  -r | --remove | --uninstall)
    UNINSTALL=1
    shift 1
    ;;
  --agents)
    AGENTS=1
    shift 1
    ;;
  --remote)
    REMOTE=1
    shift 1
    ;;
  -h | --help)
    HELP=1
    shift 1
    ;;
  -p=* | --platform=*)
    PLATFORM="${1#*=}"
    shift 1
    ;;
  -b=* | --bin-dir=*)
    BIN_DIR="${1#*=}"
    shift 1
    ;;
  -a=* | --arch=*)
    ARCH="${1#*=}"
    shift 1
    ;;
  -B=* | --base-url=*)
    BASE_URL="${1#*=}"
    shift 1
    ;;
  -V=* | --verbose=*)
    VERBOSE="${1#*=}"
    shift 1
    ;;
  -f=* | -y=* | --force=* | --yes=*)
    FORCE="${1#*=}"
    shift 1
    ;;

  *)
    error "Unknown option: $1"
    exit 1
    ;;
  esac
done

# non-empty VERBOSE enables verbose untarring
if [ -n "${VERBOSE-}" ]; then
  VERBOSE=v
else
  VERBOSE=
fi

write_env_files() {
  local quoted_railway_home
  local fish_railway_home

  if ! bin_dir_uses_railway_home; then
    return 0
  fi

  if ! mkdir -p "$RAILWAY_HOME_DIR"; then
    warn "Could not create $(tildify "$RAILWAY_HOME_DIR"); skipping activation file."
    return 0
  fi

  RAILWAY_ENV_FILE="$RAILWAY_HOME_DIR/env"
  RAILWAY_FISH_ENV_FILE="$RAILWAY_HOME_DIR/env.fish"

  if [ -e "$RAILWAY_ENV_FILE" ] && { [ ! -f "$RAILWAY_ENV_FILE" ] || [ ! -w "$RAILWAY_ENV_FILE" ]; }; then
    warn "Could not write $(tildify "$RAILWAY_ENV_FILE"); skipping activation file."
    RAILWAY_ENV_FILE=""
    RAILWAY_FISH_ENV_FILE=""
    return 0
  fi

  quoted_railway_home="$(shell_quote "$RAILWAY_HOME_DIR")"
  fish_railway_home="$(fish_quote "$RAILWAY_HOME_DIR")"

  if ! {
    printf 'export RAILWAY_HOME=%s\n' "$quoted_railway_home"
    printf 'case ":$PATH:" in\n'
    printf '  *":$RAILWAY_HOME/bin:"*) ;;\n'
    printf '  *) export PATH="$RAILWAY_HOME/bin:$PATH" ;;\n'
    printf 'esac\n'
  } > "$RAILWAY_ENV_FILE"; then
    warn "Could not write $(tildify "$RAILWAY_ENV_FILE"); skipping activation file."
    RAILWAY_ENV_FILE=""
    RAILWAY_FISH_ENV_FILE=""
    return 0
  fi

  if [ -e "$RAILWAY_FISH_ENV_FILE" ] && { [ ! -f "$RAILWAY_FISH_ENV_FILE" ] || [ ! -w "$RAILWAY_FISH_ENV_FILE" ]; }; then
    warn "Could not write $(tildify "$RAILWAY_FISH_ENV_FILE"); fish users may need to add $(tildify "$BIN_DIR") to PATH manually."
    RAILWAY_FISH_ENV_FILE=""
    return 0
  fi

  if ! {
    printf 'set -gx RAILWAY_HOME %s\n' "$fish_railway_home"
    printf 'if not contains "$RAILWAY_HOME/bin" $PATH\n'
    printf '  set -gx PATH "$RAILWAY_HOME/bin" $PATH\n'
    printf 'end\n'
  } > "$RAILWAY_FISH_ENV_FILE"; then
    warn "Could not write $(tildify "$RAILWAY_FISH_ENV_FILE"); fish users may need to add $(tildify "$BIN_DIR") to PATH manually."
    RAILWAY_FISH_ENV_FILE=""
  fi
}

print_path_commands() {
  local commands="$1"
  local command

  printf '%s\n' "$commands" | while IFS= read -r command; do
    printf '  %s\n' "$command"
  done
}

activation_command() {
  local shell_name=""
  local env_file="$RAILWAY_ENV_FILE"

  if [ -n "${SHELL-}" ]; then
    shell_name="$(basename "$SHELL")"
  fi

  if [ "$shell_name" = "fish" ]; then
    if [ -n "$RAILWAY_FISH_ENV_FILE" ]; then
      env_file="$RAILWAY_FISH_ENV_FILE"
    else
      return 1
    fi

    printf 'source %s' "$(fish_source_path "$env_file")"
    return 0
  fi

  if [ -n "$env_file" ]; then
    printf 'source %s' "$(source_path "$env_file")"
    return 0
  fi

  return 1
}

configure_shell_startup() {
  local contents="$1"
  local shell_name=""
  local rc_file=""
  local rc_dir
  local tmp_file

  SHELL_STARTUP_FILE=""
  SHELL_STARTUP_ACTION=""

  if [ -z "${HOME-}" ]; then
    return 1
  fi

  if [ -n "${SHELL-}" ]; then
    shell_name="$(basename "$SHELL")"
  fi

  case "$shell_name" in
    fish)
      rc_file="$HOME/.config/fish/config.fish"
      ;;
    zsh)
      rc_file="$HOME/.zshrc"
      ;;
    bash)
      if [ -f "$HOME/.bash_profile" ]; then
        rc_file="$HOME/.bash_profile"
      else
        rc_file="$HOME/.bashrc"
      fi
      ;;
    *)
      return 1
      ;;
  esac

  if [ -e "$rc_file" ] && { [ ! -f "$rc_file" ] || [ ! -w "$rc_file" ]; }; then
    warn "Could not update $(tildify "$rc_file"); add $(tildify "$BIN_DIR") to PATH manually."
    return 1
  fi

  rc_dir="$(dirname "$rc_file")"
  if ! mkdir -p "$rc_dir"; then
    warn "Could not create $(tildify "$rc_dir"); add $(tildify "$BIN_DIR") to PATH manually."
    return 1
  fi

  if [ -f "$rc_file" ]; then
    if grep -qF "$RAILWAY_PATH_MARKER_BEGIN" "$rc_file"; then
      SHELL_STARTUP_ACTION="Updated"
    else
      SHELL_STARTUP_ACTION="Added"
    fi
  else
    SHELL_STARTUP_ACTION="Created"
  fi

  tmp_file="$(get_tmpfile shell)"
  if [ -f "$rc_file" ]; then
    if ! awk -v begin="$RAILWAY_PATH_MARKER_BEGIN" -v end="$RAILWAY_PATH_MARKER_END" '
      $0 == begin { skip = 1; next }
      $0 == end { skip = 0; next }
      !skip { print }
    ' "$rc_file" > "$tmp_file"; then
      rm -f "$tmp_file"
      warn "Could not update $(tildify "$rc_file"); add $(tildify "$BIN_DIR") to PATH manually."
      return 1
    fi
  else
    : > "$tmp_file"
  fi

  {
    printf '\n%s\n' "$RAILWAY_PATH_MARKER_BEGIN"
    printf '%s\n' "$contents"
    printf '%s\n' "$RAILWAY_PATH_MARKER_END"
  } >> "$tmp_file"

  if [ -f "$rc_file" ]; then
    if ! cat "$tmp_file" > "$rc_file"; then
      rm -f "$tmp_file"
      warn "Could not update $(tildify "$rc_file"); add $(tildify "$BIN_DIR") to PATH manually."
      return 1
    fi
    rm -f "$tmp_file"
  elif ! mv "$tmp_file" "$rc_file"; then
    rm -f "$tmp_file"
    warn "Could not create $(tildify "$rc_file"); add $(tildify "$BIN_DIR") to PATH manually."
    return 1
  fi

  SHELL_STARTUP_FILE="$rc_file"
  return 0
}

configure_shell_path() {
  local quoted_bin_dir
  local quoted_railway_home
  local fish_bin_dir
  local fish_railway_home
  local bash_line
  local bash_contents
  local fish_line
  local fish_contents
  local path_commands
  local startup_contents
  local activation
  local shell_name

  if bin_dir_uses_railway_home; then
    quoted_railway_home="$(shell_quote "$RAILWAY_HOME_DIR")"
    fish_railway_home="$(fish_quote "$RAILWAY_HOME_DIR")"
    bash_line='export PATH="$RAILWAY_HOME/bin:$PATH"'
    bash_contents="export RAILWAY_HOME=$quoted_railway_home
$bash_line"
    fish_line='set -gx PATH "$RAILWAY_HOME/bin" $PATH'
    fish_contents="set -gx RAILWAY_HOME $fish_railway_home
$fish_line"
  else
    quoted_bin_dir="$(shell_quote "$BIN_DIR")"
    fish_bin_dir="$(fish_quote "$BIN_DIR")"
    bash_line="export PATH=$quoted_bin_dir:\"\$PATH\""
    bash_contents="$bash_line"
    fish_line="set -gx PATH $fish_bin_dir \$PATH"
    fish_contents="$fish_line"
  fi

  shell_name=""
  if [ -n "${SHELL-}" ]; then
    shell_name="$(basename "$SHELL")"
  fi

  path_commands="$bash_contents"
  startup_contents="$bash_contents"
  if [ "$shell_name" = "fish" ]; then
    path_commands="$fish_contents"
    startup_contents="$fish_contents"
  fi

  if activation="$(activation_command)"; then
    path_commands="$activation"
    startup_contents="$activation"
  fi

  warn "Railway was installed to $(tildify "$BIN_DIR"), but this shell does not resolve 'railway' from there yet."
  if configure_shell_startup "$startup_contents"; then
    info "$SHELL_STARTUP_ACTION Railway PATH setup in $(tildify "$SHELL_STARTUP_FILE")"
    info "New terminals will have railway available automatically."
  else
    info "To make railway available in new terminals, add this command to your shell startup file:"
    print_path_commands "$startup_contents"
  fi
  info "To use railway in this terminal, run:"
  print_path_commands "$path_commands"
  PATH_ACTIVATION_PRINTED=1
}

installed_railway_is_on_path() {
  local railway_on_path
  local installed_bin

  railway_on_path="$(command -v railway 2>/dev/null || true)"
  installed_bin="${BIN_DIR%/}/railway"

  [ -n "$railway_on_path" ] || return 1
  [ "$railway_on_path" = "$installed_bin" ] && return 0
  [ -e "$railway_on_path" ] && [ -e "$installed_bin" ] && [ "$railway_on_path" -ef "$installed_bin" ]
}

extract_railway_version() {
  # Pull X.Y.Z (with optional pre-release suffix and leading v) out of
  # `--version` output. Echoes empty string on failure so callers can
  # treat missing versions as "skip upgrade check".
  printf '%s' "$1" | grep -Eo 'v?[0-9]+\.[0-9]+\.[0-9]+([.-][A-Za-z0-9.-]+)?' | head -1 | sed 's/^v//'
}

version_lt() {
  # Returns 0 (true) when $1 is older than $2 by major.minor.patch.
  # Pre-release suffixes are stripped before comparison so "4.55.0"
  # beats "4.55.0-rc.1". Returns 1 (false) for equal, newer, or
  # unparseable inputs — i.e. errs on the side of NOT triggering an
  # upgrade when we can't tell.
  local a b a1 a2 a3 b1 b2 b3
  if [ -z "${1-}" ] || [ -z "${2-}" ]; then return 1; fi
  a="${1%%-*}"
  b="${2%%-*}"
  a1="$(printf '%s' "$a" | cut -d. -f1)"
  a2="$(printf '%s' "$a" | cut -d. -f2)"
  a3="$(printf '%s' "$a" | cut -d. -f3)"
  b1="$(printf '%s' "$b" | cut -d. -f1)"
  b2="$(printf '%s' "$b" | cut -d. -f2)"
  b3="$(printf '%s' "$b" | cut -d. -f3)"
  a1=${a1:-0}; a2=${a2:-0}; a3=${a3:-0}
  b1=${b1:-0}; b2=${b2:-0}; b3=${b3:-0}
  case "$a1$a2$a3$b1$b2$b3" in *[!0-9]*) return 1 ;; esac
  if [ "$a1" -lt "$b1" ]; then return 0; fi
  if [ "$a1" -gt "$b1" ]; then return 1; fi
  if [ "$a2" -lt "$b2" ]; then return 0; fi
  if [ "$a2" -gt "$b2" ]; then return 1; fi
  if [ "$a3" -lt "$b3" ]; then return 0; fi
  return 1
}

find_railway_bins() {
  local seen=":"
  local dir bin
  local old_ifs="$IFS"

  IFS=':'
  set -- $PATH
  IFS="$old_ifs"

  for dir do
    [ -n "$dir" ] || continue
    bin="$dir/railway"
    [ -x "$bin" ] || continue
    case "$seen" in
      *":$bin:"*) ;;
      *)
        printf '%s\n' "$bin"
        seen="$seen$bin:"
        ;;
    esac
  done
}

print_cli_conflicts() {
  local bins_file
  local bin version
  local count=0

  bins_file="$(get_tmpfile bins)"
  find_railway_bins > "$bins_file"

  if [ ! -s "$bins_file" ]; then
    rm -f "$bins_file"
    return
  fi

  info "Railway CLI on PATH:"
  while IFS= read -r bin; do
    count=$((count + 1))
    version="$("$bin" --version 2>/dev/null || echo unknown)"
    info "  $bin ($version)"
  done < "$bins_file"
  rm -f "$bins_file"
  info "Shells use the first entry above."

  if [ "$count" -gt 1 ]; then
    warn "Multiple Railway CLI installs found. No PATH entries were reordered or removed."
  fi
}

warn_existing_path_conflict() {
  local railway_on_path
  local installed_bin

  railway_on_path="$(command -v railway 2>/dev/null || true)"
  installed_bin="${BIN_DIR%/}/railway"

  [ -n "$railway_on_path" ] || return 0
  [ -x "$installed_bin" ] || return 0

  if installed_railway_is_on_path; then
    return 0
  fi

  warn "Another Railway CLI is already on PATH: $railway_on_path"
  warn "After activation, shells will use $(tildify "$installed_bin") first."
}

run_agent_setup() {
  local railway_bin="$1"
  local yes=""
  local remote=""

  if [ -z "${RAILWAY_INSTALL_REQUEST_ID-}" ]; then
    RAILWAY_INSTALL_REQUEST_ID="install_$(od -vAn -N16 -tx1 < /dev/urandom | tr -d ' \n')"
    export RAILWAY_INSTALL_REQUEST_ID
  fi

  if [ -n "${FORCE-}" ] || [ ! -t 0 ]; then
    yes="-y"
  fi

  if [ "$REMOTE" = 1 ]; then
    remote="--remote"
  fi

  if [ -t 0 ] && [ -z "${FORCE-}" ]; then
    if "$railway_bin" whoami >/dev/null 2>&1; then
      info "Already logged in to Railway."
    else
      info "Logging in to Railway (opens a browser)..."
      "$railway_bin" login
    fi
  fi

  "$railway_bin" setup agent $yes $remote

  if ! "$railway_bin" whoami >/dev/null 2>&1; then
    warn "Next: run '$railway_bin login' to finish setup (opens a browser)."
  fi
}

if [ "$UNINSTALL" = 1 ]; then
  confirm "Are you sure you want to uninstall railway?"

  msg=""
  sudo=""
  railway_bin="$(command -v railway 2>/dev/null || true)"

  if [ -z "$railway_bin" ] && [ -x "$BIN_DIR/railway" ]; then
    railway_bin="$BIN_DIR/railway"
  fi

  if [ -z "$railway_bin" ]; then
    error "Could not find railway on PATH or at $BIN_DIR/railway"
    exit 1
  fi

  info "REMOVING railway"

  if test_writeable "$(dirname "$railway_bin")"; then
    sudo=""
    msg="Removing railway, please wait…"
  else
    warn "Escalated permissions are required to remove ${railway_bin}"
    elevate_priv
    sudo="sudo"
    msg="Removing railway as root, please wait…"
  fi

  info "$msg"
  ${sudo} rm "$railway_bin"
  ${sudo} rm -f /tmp/railway 2>/dev/null || true

  info "Removed railway"
  exit 0
  
 fi
if [ "$HELP" = 1 ]; then
    echo "${help_text}"
    exit 0
fi

RAILWAY_UPGRADE_AGENT=0

if [ "$AGENTS" = 1 ] && has railway; then
  RAILWAY_BIN="$(command -v railway)"
  RAW_VERSION="$("$RAILWAY_BIN" --version 2>/dev/null || true)"
  CURRENT_VERSION="$(extract_railway_version "$RAW_VERSION")"

  info "Railway CLI already installed: $RAILWAY_BIN (${CURRENT_VERSION:-unknown})"
  print_cli_conflicts

  NEEDS_UPGRADE=0
  if [ -n "$CURRENT_VERSION" ] && version_lt "$CURRENT_VERSION" "$RAILWAY_VERSION"; then
    NEEDS_UPGRADE=1
  fi

  if [ "$NEEDS_UPGRADE" = 0 ]; then
    info "No PATH changes were made."
    run_agent_setup "$RAILWAY_BIN"
    exit 0
  fi

  # Refuse to clobber package-manager-owned binaries; route the user
  # through the right channel and continue with the existing CLI so the
  # agent setup still runs. Catches both M-series (/opt/homebrew/*) and
  # Intel (/usr/local/Cellar/*) direct paths plus the /usr/local/bin
  # symlink Intel Homebrew creates.
  BREW_INSTALL=0
  case "$RAILWAY_BIN" in
    /opt/homebrew/*|/usr/local/Cellar/*) BREW_INSTALL=1 ;;
    /usr/local/bin/railway)
      if [ -L "$RAILWAY_BIN" ]; then
        case "$(readlink "$RAILWAY_BIN" 2>/dev/null || true)" in
          *Cellar*|*homebrew*) BREW_INSTALL=1 ;;
        esac
      fi
      ;;
  esac

  if [ "$BREW_INSTALL" = 1 ]; then
    warn "Railway CLI was installed via Homebrew."
    warn "Run 'brew upgrade railway' to update from $CURRENT_VERSION to $RAILWAY_VERSION, then re-run cli.new --agents."
    info "Continuing with $CURRENT_VERSION."
    run_agent_setup "$RAILWAY_BIN"
    exit 0
  fi

  EXISTING_BIN_DIR="$(dirname "$RAILWAY_BIN")"
  info "Newer version available: $RAILWAY_VERSION (you have $CURRENT_VERSION)"

  UPGRADE_CONFIRMED=0
  if [ -n "${FORCE-}" ] || [ ! -t 0 ]; then
    UPGRADE_CONFIRMED=1
  else
    printf "%s " "${MAGENTA}?${NO_COLOR} Upgrade Railway CLI in ${BOLD}$EXISTING_BIN_DIR${NO_COLOR}? ${BOLD}[Y/n]${NO_COLOR}"
    set +e
    read -r yn </dev/tty
    rc=$?
    set -e
    if [ $rc -ne 0 ]; then
      error "Error reading from prompt (please re-run with the '--yes' option)"
      exit 1
    fi
    case "${yn:-y}" in
      y|Y|yes|YES|Yes) UPGRADE_CONFIRMED=1 ;;
      *) UPGRADE_CONFIRMED=0 ;;
    esac
  fi

  if [ "$UPGRADE_CONFIRMED" != "1" ]; then
    info "Skipping upgrade. Continuing with $CURRENT_VERSION."
    info "No PATH changes were made."
    run_agent_setup "$RAILWAY_BIN"
    exit 0
  fi

  info "Upgrading Railway CLI in place: $EXISTING_BIN_DIR"
  BIN_DIR="$EXISTING_BIN_DIR"
  RAILWAY_UPGRADE_AGENT=1
fi

if [ -z "$BIN_DIR" ]; then
  error "Set RAILWAY_BIN_DIR or pass --bin-dir to choose an install directory."
  exit 1
fi

TARGET="$(detect_target "${ARCH}" "${PLATFORM}")"

is_build_available "${ARCH}" "${PLATFORM}" "${TARGET}"


print_configuration () {
  if [ -n "${VERBOSE}" ]; then
    printf "  %s\n" "${UNDERLINE}Configuration${NO_COLOR}"
    debug "${BOLD}Bin directory${NO_COLOR}: ${GREEN}${BIN_DIR}${NO_COLOR}"
    debug "${BOLD}Platform${NO_COLOR}:      ${GREEN}${PLATFORM}${NO_COLOR}"
    debug "${BOLD}Arch${NO_COLOR}:          ${GREEN}${ARCH}${NO_COLOR}"
    debug "${BOLD}Version${NO_COLOR}:       ${GREEN}${RAILWAY_VERSION}${NO_COLOR}"
    printf '\n'
  fi
}

print_configuration


EXT=tar.gz
if [ "${PLATFORM}" = "pc-windows-msvc" ]; then
  EXT=zip
fi

URL="${BASE_URL}/download/v${RAILWAY_VERSION}/railway-v${RAILWAY_VERSION}-${TARGET}.${EXT}"
debug "Tarball URL: ${UNDERLINE}${BLUE}${URL}${NO_COLOR}"
if [ "$RAILWAY_UPGRADE_AGENT" != "1" ]; then
  confirm "Install railway ${GREEN}${RAILWAY_VERSION}${NO_COLOR} to ${BOLD}${GREEN}${BIN_DIR}${NO_COLOR}?"
fi
mkdir -p "${BIN_DIR}" || {
  error "Failed to create installation location ${BIN_DIR}"
  exit 1
}
check_bin_dir "${BIN_DIR}"

install "${EXT}"

completed "railway was installed successfully to $(tildify "$BIN_DIR/railway")"
if [ "$RAILWAY_UPGRADE_AGENT" != "1" ]; then
  write_env_files
fi

if ! installed_railway_is_on_path; then
  configure_shell_path
fi

if [ "$AGENTS" != 1 ]; then
  warn_existing_path_conflict
fi

if [ "$AGENTS" = 1 ]; then
  RAILWAY_BIN="$BIN_DIR/railway"
  if [ ! -x "$RAILWAY_BIN" ]; then
    error "Railway CLI install did not produce $RAILWAY_BIN"
    exit 1
  fi

  export PATH="$BIN_DIR:$PATH"

  if [ "$RAILWAY_UPGRADE_AGENT" = "1" ]; then
    info "Upgraded Railway CLI to ${GREEN}${RAILWAY_VERSION}${NO_COLOR}."
  fi

  print_cli_conflicts
  run_agent_setup "$RAILWAY_BIN"

  if [ "$RAILWAY_UPGRADE_AGENT" != "1" ] && [ "$PATH_ACTIVATION_PRINTED" != "1" ] && activation="$(activation_command)"; then
    warn "IMPORTANT: Railway was installed to $BIN_DIR."
    warn "Run '$activation' in new shells before calling railway, or add it to your shell startup file."
  elif [ "$RAILWAY_UPGRADE_AGENT" != "1" ] && [ "$PATH_ACTIVATION_PRINTED" != "1" ]; then
    warn "IMPORTANT: Railway was installed to $BIN_DIR."
    warn "Add it to PATH before calling railway in new shells."
  fi
fi

printf "$MAGENTA"
  cat <<'EOF'
                   .
         /^\     .
    /\   "V"
   /__\   I      O  o             
  //..\\  I     .                             Poof!
  \].`[/  I
  /l\/j\  (]    .  O
 /. ~~ ,\/I          .               Railway is now installed
 \\L__j^\/I       o               Run `railway help` for commands
  \/--v}  I     o   .
  |    |  I   _________
  |    |  I c(`       ')o
  |    l  I   \.     ,/
_/j  L l\_!  _//^---^\\_

EOF
printf "$NO_COLOR"

info "Railway collects anonymous CLI usage data to improve the developer experience."
info "You can opt out anytime: ${BOLD}railway telemetry disable${NO_COLOR} or ${BOLD}RAILWAY_NO_TELEMETRY=1${NO_COLOR}"
