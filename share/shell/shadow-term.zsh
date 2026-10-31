# Shadow Network terminal integration for zsh (command blocks + location).
[[ -n "$SHADOW_TERM" && -o interactive && -z "$__shadow_term_hooked" ]] || return 0
__shadow_term_hooked=1
autoload -Uz add-zsh-hook
__shadow_term_precmd() {
    local ec=$?
    print -n "\e]133;D;${ec}\a\e]7;file://${HOST}${PWD// /%20}\a\e]133;A\a"
}
__shadow_term_preexec() { print -n "\e]133;C\a"; }
add-zsh-hook precmd __shadow_term_precmd
add-zsh-hook preexec __shadow_term_preexec
PS1+=$'%{\e]133;B\a%}'
