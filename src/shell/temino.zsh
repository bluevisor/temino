# Temino shell integration for zsh, sourced from Temino's .zshenv before
# the user's .zshrc. The markers are the same as bash's (see temino.bash):
# OSC 133 A/B/C/D, OSC 7 for the working directory, OSC 6973 for PS2.

__temino_precmd() {
	local ret=$?
	builtin printf '\e]133;D;%s\a\e]7;file://%s%s\a\e]133;A\a' "$ret" "${HOST:-localhost}" "$PWD"
}

# Runs last, after any theme's own precmd has set its prompt.
__temino_prompt() {
	PS1=$'%{\e]133;B\a%}'
	PS2=$'%{\e]6973;cont\a%}'
	RPS1= RPS2=
}

__temino_preexec() {
	builtin printf '\e]133;C\a'
	# Plugins add hooks lazily; keep ours first (for $?) and last (for PS1).
	precmd_functions=(__temino_precmd ${precmd_functions:#__temino_*} __temino_prompt)
}

# The first prompt comes after .zshrc: settle the hooks and options then.
__temino_init() {
	local ret=$?
	precmd_functions=(__temino_precmd ${precmd_functions:#__temino_*} __temino_prompt)
	preexec_functions=(${preexec_functions:#__temino_preexec} __temino_preexec)
	# Temino sends whole command lines; tabs are text, never completion.
	bindkey '^I' self-insert
	unset zle_bracketed_paste
	setopt no_beep
	# The inverse % for output without a final newline would land in the
	# block; Temino's blocks end cleanly either way.
	unsetopt prompt_sp
	# A correction prompt would wait for an answer nobody sees.
	unsetopt correct correct_all
	(( ret )) && (exit $ret)
	__temino_precmd
	__temino_prompt
}
precmd_functions+=(__temino_init)
export TEMINO=1
