use std::cell::RefCell;

type QuitCallback = Box<dyn FnMut() -> bool>;

pub(crate) fn invoke_quit_callback<State>(
    state: &RefCell<State>,
    callback_slot: impl Fn(&mut State) -> &mut Option<QuitCallback>,
) -> bool {
    let callback = callback_slot(&mut state.borrow_mut()).take();
    let Some(mut callback) = callback else {
        return false;
    };

    let completed = callback();
    callback_slot(&mut state.borrow_mut()).get_or_insert(callback);
    completed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[derive(Default)]
    struct State {
        quit: Option<QuitCallback>,
        observed: usize,
    }

    #[test]
    fn quit_callback_can_reenter_platform_state_and_preserves_result() {
        for result in [false, true] {
            let state = Rc::new(RefCell::new(State::default()));
            let weak = Rc::downgrade(&state);
            state.borrow_mut().quit = Some(Box::new(move || {
                weak.upgrade().unwrap().borrow_mut().observed += 1;
                result
            }));

            assert_eq!(
                invoke_quit_callback(&state, |state| &mut state.quit),
                result
            );
            assert_eq!(
                invoke_quit_callback(&state, |state| &mut state.quit),
                result
            );
            assert_eq!(state.borrow().observed, 2);
        }
    }

    #[test]
    fn quit_callback_preserves_reentrant_replacement() {
        let state = Rc::new(RefCell::new(State::default()));
        let weak = Rc::downgrade(&state);
        state.borrow_mut().quit = Some(Box::new(move || {
            let state = weak.upgrade().unwrap();
            state.borrow_mut().quit = Some(Box::new(|| false));
            true
        }));

        assert!(invoke_quit_callback(&state, |state| &mut state.quit));
        assert!(!invoke_quit_callback(&state, |state| &mut state.quit));
    }

    #[test]
    fn absent_quit_callback_does_not_report_completed_shutdown() {
        let state = RefCell::new(State::default());
        assert!(!invoke_quit_callback(&state, |state| &mut state.quit));
    }

    #[test]
    fn recursive_quit_dispatch_does_not_invoke_active_callback_again() {
        let state = Rc::new(RefCell::new(State::default()));
        let weak = Rc::downgrade(&state);
        state.borrow_mut().quit = Some(Box::new(move || {
            let state = weak.upgrade().unwrap();
            assert!(!invoke_quit_callback(&state, |state| &mut state.quit));
            state.borrow_mut().observed += 1;
            true
        }));

        assert!(invoke_quit_callback(&state, |state| &mut state.quit));
        assert_eq!(state.borrow().observed, 1);
    }
}
