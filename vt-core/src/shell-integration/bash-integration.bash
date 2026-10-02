# soksak 터미널의 bash 셸 통합. 사이드카가 내보낸 PROMPT_COMMAND 가 첫 프롬프트에서 이 파일을 읽는다.
# 셸은 로그인 셸의 시작 파일을 그대로 읽은 상태다. 이 파일은 PROMPT_COMMAND 에서 자기 항목을 지우고,
# 두 변수의 내보내기를 멈추고, OSC 133 표시를 보내는 훅을 설치한다. bash 3.2 에서도 동작한다.

_soksak_entry='_soksak_debug_trap=$(trap -p DEBUG); builtin source "$SOKSAK_BASH_INTEGRATION"'
# 사용자가 앞이나 뒤에 붙인 항목을 남긴다. 가운데의 항목은 문법이 깨지지 않도록 빈 명령(:)으로 바꾼다.
# 이번 프롬프트에서 이 파일 다음에 실행될 항목은 _soksak_install_rest 에 보관한다.
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

# 설치한 첫 프롬프트에서는 이 파일 다음의 PROMPT_COMMAND 항목이 트랩이 설치된 뒤에 실행된다.
# 그 항목의 명령 텍스트와 같은 명령은 입력한 명령이 아니다.
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

# 명령 시작(C)은 DEBUG 트랩으로만 알 수 있다. 사용자의 DEBUG 트랩이 이미 있으면 바꾸지 않고,
# 표시를 하나도 보내지 않는다. C 없이 A 만 보내면 명령 출력 중의 크기 변경이 출력을 지우기 때문이다.
# source 로 읽는 파일 안에서는 DEBUG 트랩이 해제되어 보이지 않으므로, 주입 항목이 source 전에 기록한 값을 쓴다.
_soksak_install=
[[ -n $_soksak_debug_trap ]] || _soksak_install=1
builtin unset _soksak_debug_trap
if [[ -n $_soksak_install ]]; then
    builtin unset _soksak_install
    # PROMPT_COMMAND 의 첫 항목: 사용자 명령의 종료 상태를 보관하고 되돌려 준다.
    _soksak_prompt_begin() {
        _soksak_status=$?
        return $_soksak_status
    }

    # 작업 디렉터리(OSC 7). 예약되지 않은 문자와 / 밖의 바이트를 퍼센트 인코딩한다.
    _soksak_directory() {
        builtin local LC_ALL=C encoded= char code hex index
        for (( index = 0; index < ${#PWD}; index++ )); do
            char=${PWD:index:1}
            case $char in
                [A-Za-z0-9._~/-]) encoded+=$char ;;
                # bash 3.2 의 printf 는 127 보다 큰 바이트를 부호 확장하므로 한 바이트로 자른다.
                *) builtin printf -v code '%d' "'$char"
                   builtin printf -v hex '%%%02X' $(( code & 255 ))
                   encoded+=$hex ;;
            esac
        done
        builtin printf '\e]7;file://%s%s\a' "$HOSTNAME" "$encoded"
    }

    # PROMPT_COMMAND 의 마지막 항목: 앞 명령이 있었으면 종료 상태(D), 작업 디렉터리, 그리고 프롬프트 시작(A).
    _soksak_prompt_end() {
        if [[ -n $_soksak_running ]]; then
            builtin printf '\e]133;D;%s\a' "$_soksak_status"
            _soksak_running=
        fi
        _soksak_directory
        builtin printf '\e]133;A;redraw=last\a'
        _soksak_at_prompt=1
    }

    # 프롬프트 뒤 첫 명령 앞에서 명령 시작(C)을 보낸다. PROMPT_COMMAND 의 첫 항목이 실행되면 입력한 명령
    # 없이 다음 프롬프트를 준비하는 것이다. 트랩 명령의 마지막 인수 "$_" 는 트랩 뒤에 $_ 를 되돌린다.
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
        builtin printf '\e]133;C\a'
    }

    builtin trap '_soksak_debug "$_"' DEBUG
    PROMPT_COMMAND="_soksak_prompt_begin"$'\n'"${PROMPT_COMMAND:+$PROMPT_COMMAND$'\n'}_soksak_prompt_end"
    # 이 파일은 첫 프롬프트의 PROMPT_COMMAND 안에서 실행되므로, 이번 프롬프트의 시작도 알린다. 트랩은
    # 이미 동작할 수 있으므로(시작 중 SIGWINCH 를 받은 셸에서 확인했다) 이 호출 뒤에 다른 명령을 두지 않는다.
    _soksak_prompt_end
else
    builtin unset _soksak_install
fi
