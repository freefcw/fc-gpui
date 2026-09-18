use crate::{Bounds, DisplayId, Pixels, PlatformDisplay, Point, TrayAnchor, point, px, size};
use anyhow::Result;
use core_foundation::uuid::{CFUUIDGetUUIDBytes, CFUUIDRef};
use core_graphics::display::{CGDirectDisplayID, CGDisplayBounds, CGGetActiveDisplayList};
use objc2::MainThreadMarker;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSScreen;
use objc2_foundation::{NSNumber, NSPoint, NSRect, NSString};
use uuid::Uuid;

#[derive(Debug)]
pub(crate) struct MacDisplay(pub(crate) CGDirectDisplayID);

unsafe impl Send for MacDisplay {}

impl MacDisplay {
    /// Get the screen with the given [`DisplayId`].
    pub fn find_by_id(id: DisplayId) -> Option<Self> {
        Self::all().find(|screen| screen.id() == id)
    }

    /// Get the primary screen - the one with the menu bar, and whose bottom left
    /// corner is at the origin of the AppKit coordinate system.
    pub fn primary() -> Self {
        // Instead of iterating through all active systems displays via `all()` we use the first
        // NSScreen and gets its CGDirectDisplayID, because we can't be sure that `CGGetActiveDisplayList`
        // will always return a list of active displays (machine might be sleeping).
        //
        // The following is what Chromium does too:
        //
        // https://chromium.googlesource.com/chromium/src/+/66.0.3359.158/ui/display/mac/screen_mac.mm#56
        unsafe {
            let screens = NSScreen::screens(MainThreadMarker::new_unchecked());
            let screen = screens
                .firstObject()
                .expect("AppKit returned no screens for NSScreen::screens");
            Self(screen_number(&screen))
        }
    }

    /// Obtains an iterator over all currently active system displays.
    pub fn all() -> impl Iterator<Item = Self> {
        unsafe {
            // We're assuming there aren't more than 32 displays connected to the system.
            let mut displays = Vec::with_capacity(32);
            let mut display_count = 0;
            let result = CGGetActiveDisplayList(
                displays.capacity() as u32,
                displays.as_mut_ptr(),
                &mut display_count,
            );

            if result == 0 {
                displays.set_len(display_count as usize);
                displays.into_iter().map(MacDisplay)
            } else {
                panic!("Failed to get active display list. Result: {result}");
            }
        }
    }
}

unsafe fn as_screen<'a>(screen: *mut AnyObject) -> Option<&'a NSScreen> {
    unsafe { screen.cast::<NSScreen>().as_ref() }
}

unsafe fn screen_number(screen: &NSScreen) -> CGDirectDisplayID {
    let device_description = screen.deviceDescription();
    let screen_number_key = NSString::from_str("NSScreenNumber");
    let screen_number = device_description
        .objectForKey(&screen_number_key)
        .expect("NSScreen deviceDescription missing NSScreenNumber")
        .downcast::<NSNumber>()
        .expect("NSScreenNumber should be an NSNumber");
    screen_number.as_u32()
}

pub(crate) unsafe fn display_id_for_screen(screen: *mut AnyObject) -> Option<DisplayId> {
    unsafe {
        let screen = as_screen(screen)?;
        Some(DisplayId::new(screen_number(screen) as u64))
    }
}

pub(crate) unsafe fn primary_screen_frame() -> Option<NSRect> {
    unsafe {
        let screens = NSScreen::screens(MainThreadMarker::new_unchecked());
        screens.firstObject().map(|screen| screen.frame())
    }
}

pub(crate) unsafe fn global_point_to_native_screen_point(
    position: Point<Pixels>,
) -> Option<NSPoint> {
    unsafe {
        let primary_frame = primary_screen_frame()?;
        Some(NSPoint::new(
            f64::from(position.x),
            primary_frame.size.height - f64::from(position.y),
        ))
    }
}

pub(crate) unsafe fn screen_frame_to_tray_anchor(
    screen: *mut AnyObject,
    frame: NSRect,
) -> Option<TrayAnchor> {
    unsafe {
        let screen = as_screen(screen)?;
        let screen_frame = screen.frame();
        let local_x = frame.origin.x - screen_frame.origin.x;
        let local_y =
            screen_frame.origin.y + screen_frame.size.height - frame.origin.y - frame.size.height;

        Some(TrayAnchor {
            display_id: DisplayId::new(screen_number(screen) as u64),
            bounds: Bounds::new(
                point(px(local_x as f32), px(local_y as f32)),
                size(px(frame.size.width as f32), px(frame.size.height as f32)),
            ),
        })
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayCreateUUIDFromDisplayID(display: CGDirectDisplayID) -> CFUUIDRef;
}

impl PlatformDisplay for MacDisplay {
    fn id(&self) -> DisplayId {
        DisplayId::new(self.0 as u64)
    }

    fn uuid(&self) -> Result<Uuid> {
        let cfuuid = unsafe { CGDisplayCreateUUIDFromDisplayID(self.0 as CGDirectDisplayID) };
        anyhow::ensure!(
            !cfuuid.is_null(),
            "AppKit returned a null from CGDisplayCreateUUIDFromDisplayID"
        );

        let bytes = unsafe { CFUUIDGetUUIDBytes(cfuuid) };
        Ok(Uuid::from_bytes([
            bytes.byte0,
            bytes.byte1,
            bytes.byte2,
            bytes.byte3,
            bytes.byte4,
            bytes.byte5,
            bytes.byte6,
            bytes.byte7,
            bytes.byte8,
            bytes.byte9,
            bytes.byte10,
            bytes.byte11,
            bytes.byte12,
            bytes.byte13,
            bytes.byte14,
            bytes.byte15,
        ]))
    }

    fn bounds(&self) -> Bounds<Pixels> {
        unsafe {
            // CGDisplayBounds is in "global display" coordinates, where 0 is
            // the top left of the primary display.
            let bounds = CGDisplayBounds(self.0);

            Bounds {
                origin: Default::default(),
                size: size(px(bounds.size.width as f32), px(bounds.size.height as f32)),
            }
        }
    }

    fn visible_bounds(&self) -> Bounds<Pixels> {
        unsafe {
            let Some(screen) = self.ns_screen() else {
                return self.bounds();
            };

            let screen_frame = screen.frame();
            let visible_frame = screen.visibleFrame();

            // AppKit uses a bottom-left origin. GPUI display-local coordinates
            // use top-left origin.
            let origin_y =
                screen_frame.size.height - visible_frame.origin.y - visible_frame.size.height
                    + screen_frame.origin.y;

            Bounds {
                origin: point(
                    px(visible_frame.origin.x as f32 - screen_frame.origin.x as f32),
                    px(origin_y as f32),
                ),
                size: size(
                    px(visible_frame.size.width as f32),
                    px(visible_frame.size.height as f32),
                ),
            }
        }
    }
}

impl MacDisplay {
    unsafe fn ns_screen(&self) -> Option<objc2::rc::Retained<NSScreen>> {
        unsafe {
            let screens = NSScreen::screens(MainThreadMarker::new_unchecked());
            for i in 0..screens.len() {
                let screen = screens.objectAtIndex(i);
                if screen_number(&screen) == self.0 {
                    return Some(screen);
                }
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_id_for_screen_returns_none_for_null_screen() {
        assert_eq!(unsafe { display_id_for_screen(std::ptr::null_mut()) }, None);
    }
}
