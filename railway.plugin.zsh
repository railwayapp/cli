# make sure you execute this *after* asdf or other version managers are loaded
if (( $+commands[railway] )); then
  eval "$(railway completion zsh)"
fi