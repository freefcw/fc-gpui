use crate::WindowAppearance;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSAppearanceNameVibrantDark,
    NSAppearanceNameVibrantLight,
};

pub(crate) fn from_ns_appearance(appearance: &NSAppearance) -> WindowAppearance {
    let name = appearance.name();
    // `NSAppearanceName*` are extern statics; reading them is unsafe.
    unsafe {
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
