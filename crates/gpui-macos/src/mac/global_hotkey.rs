use std::collections::HashMap;

use crate::{Keystroke, Modifiers};
use anyhow::{Result, anyhow};
use core_graphics::event::CGKeyCode;

use super::events::{
    NO_MOD, SHIFT_MOD, always_use_command_layout, apply_command_layout, chars_for_modified_key,
    normalize_shifted_key, special_key_name,
};

const CMD_KEY: u32 = 1 << 8;
const SHIFT_KEY: u32 = 1 << 9;
const OPTION_KEY: u32 = 1 << 11;
const CONTROL_KEY: u32 = 1 << 12;
const FN_KEY: u32 = 1 << 17;
const MAX_VIRTUAL_KEY_CODE: u16 = 0x7e;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NativeHotkey {
    pub(crate) key_code: u32,
    pub(crate) modifiers: u32,
}

pub(crate) struct NativeHotkeyMapper {
    key_codes_by_keystroke: HashMap<Keystroke, NativeHotkey>,
}

impl NativeHotkeyMapper {
    pub(crate) fn new() -> Self {
        let mut key_codes_by_keystroke = HashMap::new();

        for key_code in 0..=MAX_VIRTUAL_KEY_CODE {
            for actual_modifiers in hotkey_modifier_variants() {
                let candidate = semantic_hotkey_for_key_code(key_code, actual_modifiers);
                key_codes_by_keystroke
                    .entry(candidate)
                    .or_insert_with(|| NativeHotkey {
                        key_code: key_code as u32,
                        modifiers: carbon_modifiers(actual_modifiers),
                    });
            }
        }

        Self {
            key_codes_by_keystroke,
        }
    }

    pub(crate) fn hotkey_to_native(&self, keystroke: &Keystroke) -> Result<NativeHotkey> {
        self.key_codes_by_keystroke
            .get(keystroke)
            .copied()
            .ok_or_else(|| {
                anyhow!(
                    "unsupported global hotkey key {} on macOS keyboard layout",
                    keystroke.key
                )
            })
    }
}

fn hotkey_modifier_variants() -> impl Iterator<Item = Modifiers> {
    (0..32).map(|bits| Modifiers {
        control: bits & 1 != 0,
        alt: bits & 2 != 0,
        shift: bits & 4 != 0,
        platform: bits & 8 != 0,
        function: bits & 16 != 0,
    })
}

fn carbon_modifiers(modifiers: Modifiers) -> u32 {
    let mut result = 0;
    if modifiers.platform {
        result |= CMD_KEY;
    }
    if modifiers.control {
        result |= CONTROL_KEY;
    }
    if modifiers.alt {
        result |= OPTION_KEY;
    }
    if modifiers.shift {
        result |= SHIFT_KEY;
    }
    if modifiers.function {
        result |= FN_KEY;
    }
    result
}

fn special_key_for_key_code(key_code: CGKeyCode) -> Option<&'static str> {
    // Virtual key codes from Carbon/HIToolbox.
    match key_code {
        0x24 | 0x4c => Some("enter"),
        0x30 => Some("tab"),
        0x31 => Some("space"),
        0x33 => Some("backspace"),
        0x35 => Some("escape"),
        0x40 => Some("f17"),
        0x4f => Some("f18"),
        0x50 => Some("f19"),
        0x5a => Some("f20"),
        0x60 => Some("f5"),
        0x61 => Some("f6"),
        0x62 => Some("f7"),
        0x63 => Some("f3"),
        0x64 => Some("f8"),
        0x65 => Some("f9"),
        0x67 => Some("f11"),
        0x69 => Some("f13"),
        0x6a => Some("f16"),
        0x6b => Some("f14"),
        0x6d => Some("f10"),
        0x6f => Some("f12"),
        0x71 => Some("f15"),
        0x72 => Some("insert"),
        0x73 => Some("home"),
        0x74 => Some("pageup"),
        0x75 => Some("delete"),
        0x76 => Some("f4"),
        0x77 => Some("end"),
        0x78 => Some("f2"),
        0x79 => Some("pagedown"),
        0x7a => Some("f1"),
        0x7b => Some("left"),
        0x7c => Some("right"),
        0x7d => Some("down"),
        0x7e => Some("up"),
        _ => None,
    }
}

fn suppress_function_modifier_for_key_code(key_code: CGKeyCode) -> bool {
    matches!(
        key_code,
        0x40 | 0x4f
            | 0x50
            | 0x5a
            | 0x60
            | 0x61
            | 0x62
            | 0x63
            | 0x64
            | 0x65
            | 0x67
            | 0x69
            | 0x6a
            | 0x6b
            | 0x6d
            | 0x6f
            | 0x71
            | 0x72
            | 0x73
            | 0x74
            | 0x75
            | 0x76
            | 0x77
            | 0x78
            | 0x79
            | 0x7a
            | 0x7b
            | 0x7c
            | 0x7d
            | 0x7e
    )
}

fn semantic_hotkey_for_key_code(key_code: CGKeyCode, actual_modifiers: Modifiers) -> Keystroke {
    let special_key = special_key_for_key_code(key_code);
    let first_char = if special_key.is_none() {
        chars_for_modified_key(key_code, NO_MOD)
            .chars()
            .next()
            .map(|ch| ch as u32)
    } else {
        None
    };

    let control = actual_modifiers.control;
    let alt = actual_modifiers.alt;
    let mut shift = actual_modifiers.shift;
    let command = actual_modifiers.platform;
    let function = actual_modifiers.function && !suppress_function_modifier_for_key_code(key_code);

    let key = match special_key.or_else(|| special_key_name(first_char)) {
        Some(key) => key.to_string(),
        None => {
            let mut chars_ignoring_modifiers = chars_for_modified_key(key_code, NO_MOD);
            let mut chars_with_shift = chars_for_modified_key(key_code, SHIFT_MOD);

            if command || always_use_command_layout() {
                apply_command_layout(
                    key_code,
                    &mut chars_ignoring_modifiers,
                    &mut chars_with_shift,
                );
            }

            normalize_shifted_key(chars_ignoring_modifiers, chars_with_shift, &mut shift)
        }
    };

    Keystroke {
        modifiers: Modifiers {
            control,
            alt,
            shift,
            platform: command,
            function,
        },
        key,
        key_char: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_non_printable_key_codes_without_character_translation() {
        assert_eq!(
            semantic_hotkey_for_key_code(0x33, Modifiers::default()).key,
            "backspace"
        );
        assert_eq!(
            semantic_hotkey_for_key_code(0x72, Modifiers::default()).key,
            "insert"
        );
        assert_eq!(
            semantic_hotkey_for_key_code(0x73, Modifiers::default()).key,
            "home"
        );
        assert_eq!(
            semantic_hotkey_for_key_code(0x75, Modifiers::default()).key,
            "delete"
        );
        assert_eq!(
            semantic_hotkey_for_key_code(0x7b, Modifiers::default()).key,
            "left"
        );
        assert_eq!(
            semantic_hotkey_for_key_code(0x7a, Modifiers::default()).key,
            "f1"
        );
    }

    #[test]
    fn only_suppresses_function_modifier_for_standalone_function_keys() {
        let function = Modifiers {
            function: true,
            ..Modifiers::default()
        };

        assert!(
            !semantic_hotkey_for_key_code(0x7b, function)
                .modifiers
                .function
        );
        assert!(
            !semantic_hotkey_for_key_code(0x7a, function)
                .modifiers
                .function
        );
        assert!(
            semantic_hotkey_for_key_code(0x33, function)
                .modifiers
                .function
        );
    }

    #[test]
    fn mapper_uses_cached_key_code_lookup() {
        let mapper = NativeHotkeyMapper::new();
        let native = mapper
            .hotkey_to_native(&Keystroke {
                modifiers: Modifiers::default(),
                key: "backspace".into(),
                key_char: None,
            })
            .unwrap();

        assert_eq!(native.key_code, 0x33);
    }
}
