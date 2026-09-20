use crate::WindowAppearance;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSAppearanceNameVibrantDark,
    NSAppearanceNameVibrantLight,
};

pub(crate) fn from_ns_appearance(appearance: &NSAppearance) -> WindowAppearance {
    let name = appearance.name();
    if &*name == NSAppearanceNameVibrantLight {
        WindowAppearance::VibrantLight
    } else if &*name == NSAppearanceNameVibrantDark {
        WindowAppearance::VibrantDark
    } else if &*name == NSAppearanceNameAqua {
        WindowAppearance::Light
    } else if &*name == NSAppearanceNameDarkAqua {
        WindowAppearance::Dark
    } else {
        println!("unknown appearance: {}", name);
        WindowAppearance::Light
    }
}

pub(crate) unsafe fn from_native(appearance: *mut AnyObject) -> WindowAppearance {
    unsafe {
        let Some(appearance) = appearance.cast::<NSAppearance>().as_ref() else {
            return WindowAppearance::Light;
        };
        from_ns_appearance(appearance)
    }
}

pub(crate) fn to_native(appearance: WindowAppearance) -> Option<Retained<NSAppearance>> {
    // `NSAppearanceName*` are extern statics; reading them is unsafe.
    let name = unsafe {
        match appearance {
            WindowAppearance::Light => NSAppearanceNameAqua,
            WindowAppearance::Dark => NSAppearanceNameDarkAqua,
            WindowAppearance::VibrantLight => NSAppearanceNameVibrantLight,
            WindowAppearance::VibrantDark => NSAppearanceNameVibrantDark,
        }
    };
    NSAppearance::appearanceNamed(name)
}
