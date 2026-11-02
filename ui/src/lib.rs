//! shadow-ui — the 2D UI toolkit every Shadow TUI Restricted application is built on.
//!
//! * Surfaces: normal windows (xdg-toplevel), floating layer-shell panels (the side bar) and
//!   session-lock surfaces (one per monitor).
//! * Scaling resolves itself: buffers are rendered at the exact fractional scale the compositor
//!   prefers (wp_fractional_scale_v1 + wp_viewporter). If the scale the compositor reports does not
//!   match the monitor's real pixel density (scale left at 1 on a 2560x1600 14" panel, say), text
//!   is corrected automatically so it is still the intended physical size — see [`scale`].
//! * Rendering is on the CPU into shared memory: no Vulkan, no GL context per app, a few MB each.
//!   Nothing redraws unless the app asks for it, so idle apps cost 0 frames.
//! * Fonts are memory-mapped and rasterised lazily (ab_glyph), so fallback fonts (symbols, CJK)
//!   cost nothing until a character needs them.
pub mod canvas;
pub mod font;
pub mod scale;
pub mod shell;
pub mod theme;
pub mod widgets;

pub use canvas::{Canvas, Rgb};
pub use font::Fonts;
pub use shell::{run, App, Ctx, Frame, Key, LayerKeyboard, OutputDesc, Ptr, SurfaceKind};
pub use smithay_client_toolkit::reexports::calloop;
pub use smithay_client_toolkit::seat::keyboard::Keysym;
pub use smithay_client_toolkit::shell::wlr_layer::{Anchor, Layer};
pub use theme::Theme;
