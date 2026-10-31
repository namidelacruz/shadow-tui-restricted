//! Keyboard → bytes for the pty (xterm-compatible encoding).
use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers};

fn modparam(m: &Modifiers) -> u8 {
    1 + (m.shift as u8) + 2 * (m.alt as u8) + 4 * (m.ctrl as u8)
}

pub fn encode(sym: Keysym, utf8: Option<&str>, m: &Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let mp = modparam(m);
    let csi_letter = |c: char| -> Vec<u8> {
        if mp > 1 {
            format!("\x1b[1;{mp}{c}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if mp > 1 { format!("\x1b[{n};{mp}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    let ss3 = |c: char| -> Vec<u8> {
        if mp > 1 { format!("\x1b[1;{mp}{c}").into_bytes() } else { format!("\x1bO{c}").into_bytes() }
    };
    let out = match sym {
        Keysym::Up | Keysym::KP_Up => csi_letter('A'),
        Keysym::Down | Keysym::KP_Down => csi_letter('B'),
        Keysym::Right | Keysym::KP_Right => csi_letter('C'),
        Keysym::Left | Keysym::KP_Left => csi_letter('D'),
        Keysym::Home | Keysym::KP_Home => csi_letter('H'),
        Keysym::End | Keysym::KP_End => csi_letter('F'),
        Keysym::Insert | Keysym::KP_Insert => tilde(2),
        Keysym::Delete | Keysym::KP_Delete => tilde(3),
        Keysym::Page_Up | Keysym::KP_Page_Up => tilde(5),
        Keysym::Page_Down | Keysym::KP_Page_Down => tilde(6),
        Keysym::F1 => ss3('P'),
        Keysym::F2 => ss3('Q'),
        Keysym::F3 => ss3('R'),
        Keysym::F4 => ss3('S'),
        Keysym::F5 => tilde(15),
        Keysym::F6 => tilde(17),
        Keysym::F7 => tilde(18),
        Keysym::F8 => tilde(19),
        Keysym::F9 => tilde(20),
        Keysym::F10 => tilde(21),
        Keysym::F11 => tilde(23),
        Keysym::F12 => tilde(24),
        Keysym::ISO_Left_Tab => b"\x1b[Z".to_vec(),
        Keysym::Tab if m.shift => b"\x1b[Z".to_vec(),
        Keysym::Return | Keysym::KP_Enter => {
            if m.alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() }
        }
        Keysym::BackSpace => {
            let b = if m.ctrl { 0x08 } else { 0x7f };
            if m.alt { vec![0x1b, b] } else { vec![b] }
        }
        Keysym::Escape => b"\x1b".to_vec(),
        _ => {
            let s = utf8?;
            if s.is_empty() {
                return None;
            }
            let mut bytes = s.as_bytes().to_vec();
            // Ctrl+Space / Ctrl+@ -> NUL
            if m.ctrl && (sym == Keysym::space || sym == Keysym::at) {
                bytes = vec![0];
            }
            if m.alt {
                bytes.insert(0, 0x1b);
            }
            bytes
        }
    };
    Some(out)
}
