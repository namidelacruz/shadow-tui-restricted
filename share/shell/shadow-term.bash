# Shadow Network terminal integration for bash (command blocks + location).
# Sourced from ~/.bashrc by `shadownet setup`; does nothing outside shadow-term.
[[ -n "$SHADOW_TERM" && $- == *i* && -z "$__shadow_term_hooked" ]] || return 0
__shadow_term_hooked=1
__shadow_term_prompt() {
    local ec=$?
    printf '\e]133;D;%s\a' "$ec"
    printf '\e]7;file://%s%s\a' "${HOSTNAME:-localhost}" "${PWD// /%20}"
    printf '\e]133;A\a'
    return $ec
}
PROMPT_COMMAND="__shadow_term_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
PS0+=$'\e]133;C\a'
PS1+='\[\e]133;B\a\]'
