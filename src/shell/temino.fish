# Temino shell integration for fish, run by --init-command after the
# user's config. fish 4 marks prompts and commands with OSC 133 itself;
# the same markers come from here too, for older versions (Temino ignores
# the repeats). OSC 7 carries the working directory.
set -e TEMINO_INTEGRATION

# Temino draws its own prompt.
function fish_prompt
	printf '\e]133;B\a'
end
function fish_right_prompt
end
function fish_mode_prompt
end

function __temino_prompt --on-event fish_prompt
	printf '\e]7;file://%s%s\a\e]6973;path;%s\a\e]133;A\a' $hostname $PWD (string join : $PATH)
end
function __temino_preexec --on-event fish_preexec
	printf '\e]133;C\a'
end
function __temino_postexec --on-event fish_postexec
	printf '\e]133;D;%s\a' $status
end

# Temino sends whole command lines; tabs are text, never completion.
bind \t 'commandline -i \t'
set -gx TEMINO 1
