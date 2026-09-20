//! Macos screen have a y axis that goings up from the bottom of the screen and
//! an origin at the bottom left of the main display.
mod dispatcher;
mod display;
mod display_link;
mod events;
mod keyboard;

#[cfg(feature = "screen-capture")]
mod screen_capture;

mod metal_atlas;
pub mod metal_renderer;

use metal_renderer as renderer;

#[cfg(feature = "font-kit")]
mod open_type;

#[cfg(feature = "font-kit")]
mod text_system;

mod active_window;
mod auto_launch;
mod biometric;
mod dialog;
mod dock;
mod global_hotkey;
mod network;
mod os_info;
mod permissions;
mod platform;
mod power;
mod tray;
mod window;
mod window_appearance;

use objc2::encode::{Encode, Encoding, RefEncode};
use objc2_foundation::NSNotFound;
use std::ops::Range;

pub(crate) use dispatcher::*;
pub(crate) use display::*;
pub(crate) use display_link::*;
pub(crate) use keyboard::*;
pub(crate) use platform::*;
pub(crate) use window::*;

#[cfg(feature = "font-kit")]
pub(crate) use text_system::*;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
struct NSRange {
    pub location: usize,
    pub length: usize,
}

impl NSRange {
    fn invalid() -> Self {
        Self {
            location: NSNotFound as usize,
            length: 0,
        }
    }

    fn is_valid(&self) -> bool {
        self.location != NSNotFound as usize
    }

    fn to_range(self) -> Option<Range<usize>> {
        if self.is_valid() {
            let start = self.location;
            let end = start + self.length;
            Some(start..end)
        } else {
            None
        }
    }
}

impl From<Range<usize>> for NSRange {
    fn from(range: Range<usize>) -> Self {
        NSRange {
            location: range.start,
            length: range.len(),
        }
    }
}

unsafe impl Encode for NSRange {
    const ENCODING: Encoding = Encoding::Struct("_NSRange", &[usize::ENCODING, usize::ENCODING]);
}

unsafe impl RefEncode for NSRange {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}
