#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

//! Decides which client-area `WM_MOUSEMOVE` messages are synthetic.
//!
//! Windows posts `WM_MOUSEMOVE` when a window repaints under a stationary
//! cursor. Those messages repeat the last dispatched position and have no
//! button held. The cache starts empty so the first real move — including one
//! to `(0, 0)` — is kept.
//!
//! The Windows handlers apply this to the client move path only. Every other
//! event that stores a mouse position on the GPUI window (non-client moves,
//! buttons, wheels, direct manipulation, file drag) must `Sync` the same
//! cache, or the next client move back to the previous point is dropped and
//! hit testing keeps the newer position. Leave and file-drag exit `Invalidate`
//! it: the pointer is gone, so the next client move is an entry. Focus and
//! capture-loss notifications do not move the pointer; forgetting the position
//! there would deliver the repaint move this cache exists to suppress.

/// A no-button client move that repeats the last dispatched position.
///
/// `cached == None` is never synthetic: no position has been dispatched yet.
pub(crate) fn is_synthetic_client_move<T: PartialEq>(
    cached: Option<&T>,
    new_position: &T,
    button_held: bool,
) -> bool {
    !button_held && cached == Some(new_position)
}

/// How an input event changes the last position dispatched to GPUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MouseCacheUpdate<T> {
    /// This event stores `T` as GPUI's mouse position.
    Sync(T),
    /// The pointer left the window. The next client move must be delivered.
    Invalidate,
    /// Focus or capture loss. GPUI's mouse position is unchanged.
    Unchanged,
}

pub(crate) fn apply_mouse_cache_update<T>(
    cached: Option<T>,
    update: MouseCacheUpdate<T>,
) -> Option<T> {
    match update {
        MouseCacheUpdate::Sync(position) => Some(position),
        MouseCacheUpdate::Invalidate => None,
        MouseCacheUpdate::Unchanged => cached,
    }
}

#[cfg(test)]
mod tests {
    use super::{MouseCacheUpdate, apply_mouse_cache_update, is_synthetic_client_move};

    fn client_move(
        cached: Option<(i32, i32)>,
        position: (i32, i32),
        button_held: bool,
    ) -> Option<(i32, i32)> {
        if is_synthetic_client_move(cached.as_ref(), &position, button_held) {
            cached
        } else {
            apply_mouse_cache_update(cached, MouseCacheUpdate::Sync(position))
        }
    }

    #[test]
    fn first_move_to_origin_is_delivered() {
        let cached = client_move(None, (0, 0), false);
        assert_eq!(cached, Some((0, 0)));
    }

    #[test]
    fn repeat_without_a_button_is_suppressed() {
        let cached = Some((50, 10));
        assert!(is_synthetic_client_move(cached.as_ref(), &(50, 10), false));
        assert_eq!(client_move(cached, (50, 10), false), Some((50, 10)));
    }

    #[test]
    fn repeat_with_a_button_held_is_delivered() {
        let cached = client_move(Some((50, 10)), (50, 10), true);
        assert_eq!(cached, Some((50, 10)));
    }

    #[test]
    fn return_from_non_client_is_not_a_duplicate() {
        // Client (50, 10) → title bar (-2, 10) → client (50, 10).
        let cached = client_move(None, (50, 10), false);
        let cached = apply_mouse_cache_update(cached, MouseCacheUpdate::Sync((-2, 10)));
        let cached = client_move(cached, (50, 10), false);
        assert_eq!(cached, Some((50, 10)));
        assert!(is_synthetic_client_move(cached.as_ref(), &(50, 10), false));
    }

    #[test]
    fn leave_then_reentry_at_the_same_point_is_delivered() {
        let cached = client_move(None, (50, 10), false);
        let cached = apply_mouse_cache_update(cached, MouseCacheUpdate::Invalidate);
        assert_eq!(cached, None);
        assert_eq!(client_move(cached, (50, 10), false), Some((50, 10)));
    }

    #[test]
    fn focus_and_capture_loss_keep_the_cached_point() {
        let cached = Some((50, 10));
        let cached = apply_mouse_cache_update(cached, MouseCacheUpdate::Unchanged);
        assert_eq!(cached, Some((50, 10)));
        assert!(is_synthetic_client_move(cached.as_ref(), &(50, 10), false));
    }
}
