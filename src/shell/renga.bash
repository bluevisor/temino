# Renga shell integration for bash, loaded with --rcfile.
#
# Renga draws its own prompt and line editor, so the shell's prompt is
# reduced to invisible markers (OSC 133, as in iTerm2, WezTerm and Ghostty):
#   A  prompt starts        B  prompt ends (the shell is waiting)
#   C  command output starts
#   D  command finished, with its exit status
# plus OSC 7 for the working directory and OSC 6973 for PS2 continuations.

# A login shell's startup files, since --rcfile replaces them.
[ -r /etc/profile ] && . /etc/profile
__renga_login=
for __renga_rc in ~/.bash_profile ~/.bash_login ~/.profile; do
	if [ -r "$__renga_rc" ]; then
		. "$__renga_rc"
		__renga_login=1
		break
	fi
done
# Without a login file to source it, read ~/.bashrc as an interactive
# shell normally would.
[ -z "$__renga_login" ] && [ -r ~/.bashrc ] && . ~/.bashrc
unset __renga_rc __renga_login

# Renga sends whole command lines; tabs are text, never completion.
bind 'TAB: self-insert' 2>/dev/null
bind 'set enable-bracketed-paste off' 2>/dev/null
bind 'set bell-style none' 2>/dev/null
shopt -s checkwinsize

__renga_precmd() {
	local status=$?
	builtin printf '\e]133;D;%s\a\e]7;file://%s%s\a\e]133;A\a' \
		"$status" "${HOSTNAME:-localhost}" "$PWD"
	return $status
}

if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
	PROMPT_COMMAND=(__renga_precmd "${PROMPT_COMMAND[@]}")
else
	PROMPT_COMMAND="__renga_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
fi
PS1='\[\e]133;B\a\]'
PS2='\[\e]6973;cont\a\]'
PS0='\e]133;C\a'
export RENGA=1
