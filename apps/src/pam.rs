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
const PAM_CONV_ERR: c_int = 19;
const PAM_REFRESH_CRED: c_int = 0x0010;

extern "C" fn conv(n: c_int, msgs: *mut *const PamMessage, resp: *mut *mut PamResponse, data: *mut c_void) -> c_int {
    unsafe {
        if n <= 0 || n > 32 {
            return PAM_CONV_ERR;
        }
        let pass = &*(data as *const CString);
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
                PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => libc::strdup(pass.as_ptr()),
                _ => std::ptr::null_mut(),
            };
        }
        *resp = out;
        PAM_SUCCESS
    }
}

/// Authenticate `user` with `password` against PAM service `service`.
pub fn authenticate(service: &str, user: &str, password: &str) -> Result<(), String> {
    let svc = CString::new(service).map_err(|e| e.to_string())?;
    let usr = CString::new(user).map_err(|e| e.to_string())?;
    let pw = Box::new(CString::new(password).map_err(|e| e.to_string())?);
    let c = PamConv { conv, appdata_ptr: &*pw as *const CString as *mut c_void };
    let mut h: *mut c_void = std::ptr::null_mut();
    unsafe {
        let r = pam_start(svc.as_ptr(), usr.as_ptr(), &c, &mut h);
        if r != PAM_SUCCESS {
            return Err(format!("pam_start failed ({r})"));
        }
        let mut r = pam_authenticate(h, 0);
        if r == PAM_SUCCESS {
            r = pam_acct_mgmt(h, 0);
        }
        if r == PAM_SUCCESS {
            pam_setcred(h, PAM_REFRESH_CRED);
        }
        let msg = if r == PAM_SUCCESS { None } else { Some(CStr::from_ptr(pam_strerror(h, r)).to_string_lossy().into_owned()) };
        pam_end(h, r);
        drop(pw);
        match msg {
            None => Ok(()),
            Some(m) => Err(m),
        }
    }
}
