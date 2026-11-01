//! Minimal PAM client for the lock screen (password conversation only). Runs on a worker thread,
//! because a failed attempt deliberately takes a couple of seconds.
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}
#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}
#[repr(C)]
struct PamConv {
    conv: extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
extern "C" {
    fn pam_start(service: *const c_char, user: *const c_char, conv: *const PamConv, pamh: *mut *mut c_void) -> c_int;
    fn pam_authenticate(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_acct_mgmt(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_setcred(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut c_void, status: c_int) -> c_int;
    fn pam_strerror(pamh: *mut c_void, errnum: c_int) -> *const c_char;
}

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
pub const PAM_PERM_DENIED: c_int = 6;
pub const PAM_AUTH_ERR: c_int = 7;
pub const PAM_AUTHINFO_UNAVAIL: c_int = 9;
pub const PAM_USER_UNKNOWN: c_int = 10;
pub const PAM_MAXTRIES: c_int = 11;
pub const PAM_NEW_AUTHTOK_REQD: c_int = 12;
pub const PAM_ACCT_EXPIRED: c_int = 13;

/// What the conversation needs: the password, and room for what PAM tells the user
/// ("The account is locked due to 3 failed logins.", "Place your finger…").
struct ConvData {
    pass: CString,
    said: std::cell::RefCell<Vec<String>>,
}

/// A failed PAM transaction: the PAM code, its text, and PAM's own messages to the user.
#[derive(Debug, Clone)]
pub struct Failure {
    pub code: i32,
    pub text: String,
    pub messages: Vec<String>,
    /// failed at the account stage (expired / locked account) rather than the password check
    pub account_stage: bool,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.messages.is_empty() {
            write!(f, "{}", self.text)
        } else {
            write!(f, "{} — {}", self.text, self.messages.join(" "))
        }
    }
}
const PAM_CONV_ERR: c_int = 19;
const PAM_REFRESH_CRED: c_int = 0x0010;

extern "C" fn conv(n: c_int, msgs: *mut *const PamMessage, resp: *mut *mut PamResponse, data: *mut c_void) -> c_int {
    unsafe {
        if n <= 0 || n > 32 {
            return PAM_CONV_ERR;
        }
        let d = &*(data as *const ConvData);
        let out = libc::calloc(n as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
        if out.is_null() {
            return PAM_CONV_ERR;
        }
        for i in 0..n as isize {
            // Linux-PAM passes an array of pointers
            let m = &**msgs.offset(i);
            let r = &mut *out.offset(i);
            r.resp_retcode = 0;
            r.resp = match m.msg_style {
                PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => libc::strdup(d.pass.as_ptr()),
                PAM_ERROR_MSG | PAM_TEXT_INFO => {
                    if !m.msg.is_null() {
                        let t = CStr::from_ptr(m.msg).to_string_lossy().trim().to_string();
                        if !t.is_empty() {
                            d.said.borrow_mut().push(t);
                        }
                    }
                    std::ptr::null_mut()
                }
                _ => std::ptr::null_mut(),
            };
        }
        *resp = out;
        PAM_SUCCESS
    }
}

/// Authenticate `user` with `password` against PAM service `service`.
pub fn authenticate(service: &str, user: &str, password: &str) -> Result<(), String> {
    authenticate_detailed(service, user, password, true).map_err(|f| f.to_string())
}

/// Like `authenticate`, with the PAM code and PAM's own messages. `setcred` refreshes credentials
/// (the lock screen); a password test leaves them alone.
pub fn authenticate_detailed(service: &str, user: &str, password: &str, setcred: bool) -> Result<(), Failure> {
    let mk = |code: i32, text: String| Failure { code, text, messages: vec![], account_stage: false };
    let svc = CString::new(service).map_err(|e| mk(-1, e.to_string()))?;
    let usr = CString::new(user).map_err(|e| mk(-1, e.to_string()))?;
    let data = Box::new(ConvData { pass: CString::new(password).map_err(|e| mk(-1, e.to_string()))?, said: Default::default() });
    let c = PamConv { conv, appdata_ptr: &*data as *const ConvData as *mut c_void };
    let mut h: *mut c_void = std::ptr::null_mut();
    unsafe {
        let r = pam_start(svc.as_ptr(), usr.as_ptr(), &c, &mut h);
        if r != PAM_SUCCESS {
            return Err(mk(r, format!("pam_start failed ({r}) — is /etc/pam.d/{service} missing?")));
        }
        let mut account_stage = false;
        let mut r = pam_authenticate(h, 0);
        if r == PAM_SUCCESS {
            r = pam_acct_mgmt(h, 0);
            account_stage = r != PAM_SUCCESS;
        }
        if r == PAM_SUCCESS && setcred {
            pam_setcred(h, PAM_REFRESH_CRED);
        }
        let text = if r == PAM_SUCCESS { String::new() } else { CStr::from_ptr(pam_strerror(h, r)).to_string_lossy().into_owned() };
        pam_end(h, r);
        let messages = data.said.borrow().clone();
        drop(data);
        if r == PAM_SUCCESS {
            Ok(())
        } else {
            Err(Failure { code: r, text, messages, account_stage })
        }
    }
}
