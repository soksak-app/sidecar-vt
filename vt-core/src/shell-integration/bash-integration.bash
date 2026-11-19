# The bash shell integration of the soksak terminal. The PROMPT_COMMAND that the sidecar exports reads this file at the first prompt.
# The shell has read the startup files of the login shell unchanged. This file removes its own entry from PROMPT_COMMAND,
# stops the export of the two variables and installs the hooks that send the OSC 133 marks. It works in bash 3.2 too.

_soksak_entry='_soksak_debug_trap=$(trap -p DEBUG); builtin source "$SOKSAK_BASH_INTEGRATION"'
# Entries that the user added before or after the entry stay. An entry in the middle becomes the empty command (:), so
# that the syntax stays valid.
# The entries that run after this file at this prompt are kept in _soksak_install_rest.
case $PROMPT_COMMAND in
    "$_soksak_entry")
        _soksak_install_rest=
        PROMPT_COMMAND= ;;
    "$_soksak_entry"$'\n'*)
        PROMPT_COMMAND=${PROMPT_COMMAND#"$_soksak_entry"$'\n'}
        _soksak_install_rest=$PROMPT_COMMAND ;;
    "$_soksak_entry;"*)
        PROMPT_COMMAND=${PROMPT_COMMAND#"$_soksak_entry;"}
        _soksak_install_rest=$PROMPT_COMMAND ;;
    *)
        _soksak_install_rest=${PROMPT_COMMAND#*"$_soksak_entry"}
        PROMPT_COMMAND=${PROMPT_COMMAND/"$_soksak_entry"/:} ;;
esac
builtin unset _soksak_entry
builtin export -n PROMPT_COMMAND
builtin unset SOKSAK_BASH_INTEGRATION

_soksak_trim() {
    _soksak_trimmed=${1#"${1%%[![:space:]]*}"}
    _soksak_trimmed=${_soksak_trimmed%"${_soksak_trimmed##*[![:space:]]}"}
}

# At the first prompt of the installation, the PROMPT_COMMAND entries after this file run after the trap is installed.
# A command with the same text as one of those entries is not a command that the user typed.
_soksak_in_install_rest() {
    builtin local IFS=$'\n;' entry
    builtin local -a entries
    builtin read -r -d '' -a entries <<< "$_soksak_install_rest"
    _soksak_trim "$1"
    builtin local command=$_soksak_trimmed
    for entry in "${entries[@]}"; do
        _soksak_trim "$entry"
        [[ $_soksak_trimmed == "$command" ]] && return 0
    done
    return 1
}

# Only the DEBUG trap tells the start of a command (C). When the user already has a DEBUG trap, the file keeps it and
# sends no mark at all, because A without C lets a resize during the output of a command erase that output.
# Inside a file read with source the DEBUG trap is unset and not visible, so the file uses the value that the injected
# entry recorded before source.
_soksak_install=
[[ -n $_soksak_debug_trap ]] || _soksak_install=1
builtin unset _soksak_debug_trap
if [[ -n $_soksak_install ]]; then
    builtin unset _soksak_install
    # The marks of the prompt lines (core docs/spec/terminal-state.md#prompt-region): P;k=s after each line break of PS1,
    # both the prompt escape \n and a newline character. After a resize readline draws the last line of the prompt again
    # with its mark, so the line that it draws records its row. A command and the PROMPT_COMMAND entries before the next
    # prompt see the PS1 of the startup files.
    _soksak_mark() {
        builtin local mark='\[\e]133;P;k=s\a\]' newline=$'\n'
        _soksak_user_ps1=$PS1
        PS1=${PS1//\\n/\\n$mark}
        PS1=${PS1//$newline/$newline$mark}
        _soksak_marked=1
    }
    _soksak_unmark() {
        [[ -n $_soksak_marked ]] || return 0
        PS1=$_soksak_user_ps1
        _soksak_marked=
    }

    # The first entry of PROMPT_COMMAND: it keeps the exit status of the command of the user and returns it.
    # The entry also restores the PS1 of the startup files, because an empty input line reaches it without a command.
    _soksak_prompt_begin() {
        _soksak_status=$?
        _soksak_unmark
        return $_soksak_status
    }

    # The working directory (OSC 7). Bytes other than the unreserved characters and / are percent-encoded.
    _soksak_directory() {
        builtin local LC_ALL=C encoded= char code hex index
        for (( index = 0; index < ${#PWD}; index++ )); do
            char=${PWD:index:1}
            case $char in
                [A-Za-z0-9._~/-]) encoded+=$char ;;
                # printf of bash 3.2 extends the sign of a byte above 127, so the value is cut to one byte.
                *) builtin printf -v code '%d' "'$char"
                   builtin printf -v hex '%%%02X' $(( code & 255 ))
                   encoded+=$hex ;;
            esac
        done
        builtin printf '\e]7;file://%s%s\a' "$HOSTNAME" "$encoded"
    }

    # The last entry of PROMPT_COMMAND: the exit status (D) when a command ran before, the working directory, and the prompt start (A).
    _soksak_prompt_end() {
        if [[ -n $_soksak_running ]]; then
            builtin printf '\e]133;D;%s\a' "$_soksak_status"
            _soksak_running=
        fi
        _soksak_directory
        builtin printf '\e]133;A;redraw=last\a'
        _soksak_mark
        _soksak_at_prompt=1
    }

    # Sends the command start (C) before the first command after the prompt. When the first entry of PROMPT_COMMAND runs, the shell
    # prepares the next prompt without a typed command. The last argument "$_" of the trap command restores $_ after the trap.
    _soksak_debug() {
        if [[ $BASH_COMMAND == _soksak_prompt_begin ]]; then
            _soksak_at_prompt=
            _soksak_install_rest=
            return 0
        fi
        [[ -n $_soksak_at_prompt ]] || return 0
        if [[ -n $_soksak_install_rest ]] && _soksak_in_install_rest "$BASH_COMMAND"; then
            return 0
        fi
        _soksak_install_rest=
        _soksak_at_prompt=
        _soksak_running=1
        _soksak_unmark
        builtin printf '\e]133;C\a'
    }

    builtin trap '_soksak_debug "$_"' DEBUG
    PROMPT_COMMAND="_soksak_prompt_begin"$'\n'"${PROMPT_COMMAND:+$PROMPT_COMMAND$'\n'}_soksak_prompt_end"
    # This file runs inside the PROMPT_COMMAND of the first prompt, so it also reports the start of this prompt. The trap
    # can already run (a shell that received SIGWINCH during its start ran it), so no other command follows this call.
    _soksak_prompt_end
else
    builtin unset _soksak_install
fi
