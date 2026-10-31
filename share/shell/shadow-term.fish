# Shadow Network terminal integration for fish (command blocks + location).
if not set -q SHADOW_TERM; or not status is-interactive; or set -q __shadow_term_hooked
    exit 0
end
set -g __shadow_term_hooked 1
function __shadow_term_prompt --on-event fish_prompt
    printf '\e]133;D;%s\a\e]7;file://%s%s\a\e]133;A\a' $__shadow_term_status (hostname) (string replace -a ' ' '%20' $PWD)
end
function __shadow_term_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end
function __shadow_term_postexec --on-event fish_postexec
    set -g __shadow_term_status $status
end
set -g __shadow_term_status 0
