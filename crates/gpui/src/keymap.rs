mod binding;
mod context;

pub use binding::*;
pub use context::*;

use crate::{Action, AsKeystroke, Keystroke, is_no_action};
use collections::HashMap;
use smallvec::SmallVec;
use std::any::TypeId;

/// An opaque identifier of which version of the keymap is currently active.
/// The keymap's version is changed whenever bindings are added or removed.
#[derive(Copy, Clone, Eq, PartialEq, Default)]
pub struct KeymapVersion(usize);

/// A collection of key bindings for the user's application.
#[derive(Default)]
pub struct Keymap {
    bindings: Vec<KeyBinding>,
    binding_indices_by_action_id: HashMap<TypeId, SmallVec<[usize; 3]>>,
    no_action_binding_indices: Vec<usize>,
    version: KeymapVersion,
}

/// Index of a binding within a keymap.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct BindingIndex(usize);

fn is_user_binding(binding: &KeyBinding) -> bool {
    // Bindings without source metadata retain the user-binding behavior.
    binding.meta.is_none_or(KeyBindingMetaIndex::is_user)
}

impl Keymap {
    /// Create a new keymap with the given bindings.
    pub fn new(bindings: Vec<KeyBinding>) -> Self {
        let mut this = Self::default();
        this.add_bindings(bindings);
        this
    }

    /// Get the current version of the keymap.
    pub fn version(&self) -> KeymapVersion {
        self.version
    }

    /// Add more bindings to the keymap.
    pub fn add_bindings<T: IntoIterator<Item = KeyBinding>>(&mut self, bindings: T) {
        for binding in bindings {
            let action_id = binding.action().as_any().type_id();
            if is_no_action(&*binding.action) {
                self.no_action_binding_indices.push(self.bindings.len());
            } else {
                self.binding_indices_by_action_id
                    .entry(action_id)
                    .or_default()
                    .push(self.bindings.len());
            }
            self.bindings.push(binding);
        }

        self.version.0 += 1;
    }

    /// Reset this keymap to its initial state.
    pub fn clear(&mut self) {
        self.bindings.clear();
        self.binding_indices_by_action_id.clear();
        self.no_action_binding_indices.clear();
        self.version.0 += 1;
    }

    /// Iterate over all bindings, in the order they were added.
    pub fn bindings(&self) -> impl DoubleEndedIterator<Item = &KeyBinding> + ExactSizeIterator {
        self.bindings.iter()
    }

    /// Iterate over all bindings for the given action, in the order they were added. For display,
    /// the last binding should take precedence.
    pub fn bindings_for_action<'a>(
        &'a self,
        action: &'a dyn Action,
    ) -> impl 'a + DoubleEndedIterator<Item = &'a KeyBinding> {
        let action_id = action.type_id();
        let binding_indices = self
            .binding_indices_by_action_id
            .get(&action_id)
            .map_or(&[] as _, SmallVec::as_slice)
            .iter();

        binding_indices.filter_map(|ix| {
            let binding = &self.bindings[*ix];
            if !binding.action().partial_eq(action) {
                return None;
            }

            for null_ix in &self.no_action_binding_indices {
                if null_ix > ix {
                    let null_binding = &self.bindings[*null_ix];
                    if is_user_binding(null_binding)
                        && null_binding.keystrokes == binding.keystrokes
                    {
                        let null_binding_matches =
                            match (&null_binding.context_predicate, &binding.context_predicate) {
                                (None, _) => true,
                                (Some(_), None) => false,
                                (Some(null_predicate), Some(predicate)) => {
                                    null_predicate.is_superset(predicate)
                                }
                            };
                        if null_binding_matches {
                            return None;
                        }
                    }
                }
            }

            Some(binding)
        })
    }

    /// Returns all bindings that might match the input without checking context. The bindings
    /// returned in precedence order (reverse of the order they were added to the keymap).
    pub fn all_bindings_for_input(&self, input: &[Keystroke]) -> Vec<KeyBinding> {
        self.bindings()
            .rev()
            .filter_map(|binding| {
                binding.match_keystrokes(input).filter(|pending| !pending)?;
                Some(binding.clone())
            })
            .collect()
    }

    /// Returns a list of bindings that match the given input, and a boolean indicating whether or
    /// not more bindings might match if the input was longer. Bindings are returned in precedence
    /// order (higher precedence first, reverse of the order they were added to the keymap).
    ///
    /// Precedence is defined by the depth in the tree (matches on the Editor take precedence over
    /// matches on the Pane, then the Workspace, etc.). Bindings with no context are treated as the
    /// same as the deepest context.
    ///
    /// In the case of multiple bindings at the same depth, the ones added to the keymap later take
    /// precedence. User bindings are added after built-in bindings so that they take precedence.
    ///
    /// If a user has disabled a binding with `"x": null` it will not be returned. Disabled bindings
    /// are evaluated with the same precedence rules so you can disable a rule in a given context
    /// only.
    pub fn bindings_for_input(
        &self,
        input: &[impl AsKeystroke],
        context_stack: &[KeyContext],
    ) -> (SmallVec<[KeyBinding; 1]>, bool) {
        let (bindings, pending) = self.resolve_bindings_for_input(
            input,
            context_stack,
            self.bindings().enumerate().rev(),
        );
        (
            bindings
                .into_iter()
                .map(|(_, binding)| binding.clone())
                .collect(),
            pending,
        )
    }

    /// Same as `bindings_for_input`, but only looks at `keymap_bindings` instead of the whole
    /// keymap, and returns each binding with its keymap index.
    ///
    /// `keymap_bindings` must be in reverse keymap order and include every binding that could
    /// match `input`. For example, leaving out a `null` binding would make the binding it disables
    /// look like it wins. `bindings_for_input` passes the whole keymap, which always meets this but
    /// goes through every binding on every call. Passing a pre-filtered list is cheaper when
    /// resolving many inputs against it.
    fn resolve_bindings_for_input<'a>(
        &self,
        input: &[impl AsKeystroke],
        context_stack: &[KeyContext],
        keymap_bindings: impl IntoIterator<Item = (usize, &'a KeyBinding)>,
    ) -> (SmallVec<[(BindingIndex, &'a KeyBinding); 1]>, bool) {
        let mut matching_bindings =
            SmallVec::<[(usize, BindingIndex, &KeyBinding, bool); 1]>::new();

        for (ix, binding) in keymap_bindings {
            if is_no_action(&*binding.action) && !is_user_binding(binding) {
                continue;
            }
            let Some(depth) = self.binding_enabled(binding, context_stack) else {
                continue;
            };
            let Some(pending) = binding.match_keystrokes(input) else {
                continue;
            };

            matching_bindings.push((depth, BindingIndex(ix), binding, pending));
        }

        matching_bindings.sort_by(|(depth_a, ix_a, _, _), (depth_b, ix_b, _, _)| {
            depth_b.cmp(depth_a).then(ix_b.cmp(ix_a))
        });

        let mut bindings: SmallVec<[_; 1]> = SmallVec::new();
        let mut first_binding_index = None;

        for &(_, ix, binding, pending) in &matching_bindings {
            if pending {
                continue;
            }
            if is_no_action(&*binding.action) {
                break;
            }
            bindings.push((ix, binding));
            first_binding_index.get_or_insert(ix);
        }

        let mut full_input = SmallVec::<[&Keystroke; 4]>::new();
        let pending = matching_bindings.iter().any(|&(_, ix, binding, pending)| {
            if !pending
                || is_no_action(&*binding.action)
                || first_binding_index.is_some_and(|binding_ix| binding_ix > ix)
            {
                return false;
            }

            // Resolve this completion in the same depth/registration order as
            // a full input. Keep the typed prefix so character and physical-key
            // spellings (such as Alt-S and ß) observe the same NoAction binding.
            full_input.clear();
            full_input.extend(input.iter().map(AsKeystroke::as_keystroke));
            full_input.extend(
                binding.keystrokes[input.len()..]
                    .iter()
                    .map(AsKeystroke::as_keystroke),
            );
            matching_bindings
                .iter()
                .find(|(_, _, candidate, pending)| {
                    *pending && candidate.match_keystrokes(&full_input) == Some(false)
                })
                .is_some_and(|(_, _, winner, _)| !is_no_action(&*winner.action))
        });

        (bindings, pending)
    }

    /// Find the bindings that can follow the current input sequence, in precedence order. Only
    /// includes bindings that dispatch would run for their full keystrokes, so bindings disabled
    /// with `null` (`NoAction`) are left out.
    ///
    /// fc-gpui disables a binding with `NoAction` rather than a separate `Unbind` action.
    pub fn possible_next_bindings_for_input(
        &self,
        input: &[Keystroke],
        context_stack: &[KeyContext],
    ) -> Vec<KeyBinding> {
        // Keeps `null` bindings, which can disable candidates.
        let bindings_extending_input = self
            .bindings()
            .enumerate()
            .rev()
            .filter(|(_, binding)| {
                binding
                    .match_keystrokes(input)
                    .is_some_and(|pending| pending)
            })
            .collect::<Vec<_>>();

        let mut full_input = SmallVec::<[&Keystroke; 4]>::new();
        let mut candidates = bindings_extending_input
            .iter()
            .filter(|(_, binding)| !is_no_action(&*binding.action))
            .filter_map(|&(ix, binding)| {
                let depth = self.binding_enabled(binding, context_stack)?;
                // Use the typed input rather than the binding's own keystrokes. A typed keystroke
                // can match through its physical key, like `alt-s`, or the character it produced,
                // like `ß`.
                full_input.clear();
                full_input.extend(input.iter());
                full_input.extend(
                    binding.keystrokes[input.len()..]
                        .iter()
                        .map(AsKeystroke::as_keystroke),
                );
                let (dispatched, _) = self.resolve_bindings_for_input(
                    &full_input,
                    context_stack,
                    bindings_extending_input.iter().copied(),
                );
                dispatched
                    .iter()
                    .any(|(dispatched_ix, _)| *dispatched_ix == BindingIndex(ix))
                    .then_some((depth, ix, binding))
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by(|(depth_a, ix_a, _), (depth_b, ix_b, _)| {
            depth_b.cmp(depth_a).then(ix_b.cmp(ix_a))
        });

        candidates
            .into_iter()
            .map(|(_, _, binding)| binding.clone())
            .collect()
    }
    /// Check if the given binding is enabled, given a certain key context.
    /// Returns the deepest depth at which the binding matches, or None if it doesn't match.
    fn binding_enabled(&self, binding: &KeyBinding, contexts: &[KeyContext]) -> Option<usize> {
        if let Some(predicate) = &binding.context_predicate {
            predicate.depth_of(contexts)
        } else {
            Some(contexts.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate as gpui;
    use gpui::NoAction;

    actions!(
        test_only,
        [ActionAlpha, ActionBeta, ActionGamma, ActionDelta,]
    );

    #[test]
    fn test_keymap() {
        let bindings = [
            KeyBinding::new("ctrl-a", ActionAlpha {}, None),
            KeyBinding::new("ctrl-a", ActionBeta {}, Some("pane")),
            KeyBinding::new("ctrl-a", ActionGamma {}, Some("editor && mode==full")),
        ];

        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings.clone());

        // global bindings are enabled in all contexts
        assert_eq!(keymap.binding_enabled(&bindings[0], &[]), Some(0));
        assert_eq!(
            keymap.binding_enabled(&bindings[0], &[KeyContext::parse("terminal").unwrap()]),
            Some(1)
        );

        // contextual bindings are enabled in contexts that match their predicate
        assert_eq!(
            keymap.binding_enabled(&bindings[1], &[KeyContext::parse("barf x=y").unwrap()]),
            None
        );
        assert_eq!(
            keymap.binding_enabled(&bindings[1], &[KeyContext::parse("pane x=y").unwrap()]),
            Some(1)
        );

        assert_eq!(
            keymap.binding_enabled(&bindings[2], &[KeyContext::parse("editor").unwrap()]),
            None
        );
        assert_eq!(
            keymap.binding_enabled(
                &bindings[2],
                &[KeyContext::parse("editor mode=full").unwrap()]
            ),
            Some(1)
        );
    }

    #[test]
    fn test_depth_precedence() {
        let bindings = [
            KeyBinding::new("ctrl-a", ActionBeta {}, Some("pane")),
            KeyBinding::new("ctrl-a", ActionGamma {}, Some("editor")),
        ];

        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        let (result, pending) = keymap.bindings_for_input(
            &[Keystroke::parse("ctrl-a").unwrap()],
            &[
                KeyContext::parse("pane").unwrap(),
                KeyContext::parse("editor").unwrap(),
            ],
        );

        assert!(!pending);
        assert_eq!(result.len(), 2);
        assert!(result[0].action.partial_eq(&ActionGamma {}));
        assert!(result[1].action.partial_eq(&ActionBeta {}));
    }

    #[test]
    fn test_keymap_disabled() {
        let bindings = [
            KeyBinding::new("ctrl-a", ActionAlpha {}, Some("editor")),
            KeyBinding::new("ctrl-b", ActionAlpha {}, Some("editor")),
            KeyBinding::new("ctrl-a", NoAction {}, Some("editor && mode==full")),
            KeyBinding::new("ctrl-b", NoAction {}, None),
        ];

        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        // binding is only enabled in a specific context
        assert!(
            keymap
                .bindings_for_input(
                    &[Keystroke::parse("ctrl-a").unwrap()],
                    &[KeyContext::parse("barf").unwrap()],
                )
                .0
                .is_empty()
        );
        assert!(
            !keymap
                .bindings_for_input(
                    &[Keystroke::parse("ctrl-a").unwrap()],
                    &[KeyContext::parse("editor").unwrap()],
                )
                .0
                .is_empty()
        );

        // binding is disabled in a more specific context
        assert!(
            keymap
                .bindings_for_input(
                    &[Keystroke::parse("ctrl-a").unwrap()],
                    &[KeyContext::parse("editor mode=full").unwrap()],
                )
                .0
                .is_empty()
        );

        // binding is globally disabled
        assert!(
            keymap
                .bindings_for_input(
                    &[Keystroke::parse("ctrl-b").unwrap()],
                    &[KeyContext::parse("barf").unwrap()],
                )
                .0
                .is_empty()
        );
    }

    #[test]
    /// Tests for https://github.com/zed-industries/zed/issues/30259
    fn test_multiple_keystroke_binding_disabled() {
        let bindings = [
            KeyBinding::new("space w w", ActionAlpha {}, Some("workspace")),
            KeyBinding::new("space w w", NoAction {}, Some("editor")),
        ];

        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        let space = || Keystroke::parse("space").unwrap();
        let w = || Keystroke::parse("w").unwrap();

        let space_w = [space(), w()];
        let space_w_w = [space(), w(), w()];

        let workspace_context = || [KeyContext::parse("workspace").unwrap()];

        let editor_workspace_context = || {
            [
                KeyContext::parse("workspace").unwrap(),
                KeyContext::parse("editor").unwrap(),
            ]
        };

        // Ensure `space` results in pending input on the workspace, but not editor
        let space_workspace = keymap.bindings_for_input(&[space()], &workspace_context());
        assert!(space_workspace.0.is_empty());
        assert!(space_workspace.1);

        let space_editor = keymap.bindings_for_input(&[space()], &editor_workspace_context());
        assert!(space_editor.0.is_empty());
        assert!(!space_editor.1);

        // Ensure `space w` results in pending input on the workspace, but not editor
        let space_w_workspace = keymap.bindings_for_input(&space_w, &workspace_context());
        assert!(space_w_workspace.0.is_empty());
        assert!(space_w_workspace.1);

        let space_w_editor = keymap.bindings_for_input(&space_w, &editor_workspace_context());
        assert!(space_w_editor.0.is_empty());
        assert!(!space_w_editor.1);

        // Ensure `space w w` results in the binding in the workspace, but not in the editor
        let space_w_w_workspace = keymap.bindings_for_input(&space_w_w, &workspace_context());
        assert!(!space_w_w_workspace.0.is_empty());
        assert!(!space_w_w_workspace.1);

        let space_w_w_editor = keymap.bindings_for_input(&space_w_w, &editor_workspace_context());
        assert!(space_w_w_editor.0.is_empty());
        assert!(!space_w_w_editor.1);

        // Now test what happens if we have another binding defined AFTER the NoAction
        // that should result in pending
        let bindings = [
            KeyBinding::new("space w w", ActionAlpha {}, Some("workspace")),
            KeyBinding::new("space w w", NoAction {}, Some("editor")),
            KeyBinding::new("space w x", ActionAlpha {}, Some("editor")),
        ];
        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        let space_editor = keymap.bindings_for_input(&[space()], &editor_workspace_context());
        assert!(space_editor.0.is_empty());
        assert!(space_editor.1);

        // Now test what happens if we have another binding defined BEFORE the NoAction
        // that should result in pending
        let bindings = [
            KeyBinding::new("space w w", ActionAlpha {}, Some("workspace")),
            KeyBinding::new("space w x", ActionAlpha {}, Some("editor")),
            KeyBinding::new("space w w", NoAction {}, Some("editor")),
        ];
        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        let space_editor = keymap.bindings_for_input(&[space()], &editor_workspace_context());
        assert!(space_editor.0.is_empty());
        assert!(space_editor.1);

        // Now test what happens if we have another binding defined at a higher context
        // that should result in pending
        let bindings = [
            KeyBinding::new("space w w", ActionAlpha {}, Some("workspace")),
            KeyBinding::new("space w x", ActionAlpha {}, Some("workspace")),
            KeyBinding::new("space w w", NoAction {}, Some("editor")),
        ];
        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        let space_editor = keymap.bindings_for_input(&[space()], &editor_workspace_context());
        assert!(space_editor.0.is_empty());
        assert!(space_editor.1);
    }

    #[test]
    fn test_override_multikey() {
        let bindings = [
            KeyBinding::new("ctrl-w left", ActionAlpha {}, Some("editor")),
            KeyBinding::new("ctrl-w", NoAction {}, Some("editor")),
        ];

        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        // Ensure `space` results in pending input on the workspace, but not editor
        let (result, pending) = keymap.bindings_for_input(
            &[Keystroke::parse("ctrl-w").unwrap()],
            &[KeyContext::parse("editor").unwrap()],
        );
        assert!(result.is_empty());
        assert!(pending);

        let bindings = [
            KeyBinding::new("ctrl-w left", ActionAlpha {}, Some("editor")),
            KeyBinding::new("ctrl-w", ActionBeta {}, Some("editor")),
        ];

        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        // Ensure `space` results in pending input on the workspace, but not editor
        let (result, pending) = keymap.bindings_for_input(
            &[Keystroke::parse("ctrl-w").unwrap()],
            &[KeyContext::parse("editor").unwrap()],
        );
        assert_eq!(result.len(), 1);
        assert!(!pending);
    }

    #[test]
    fn test_simple_disable() {
        let bindings = [
            KeyBinding::new("ctrl-x", ActionAlpha {}, Some("editor")),
            KeyBinding::new("ctrl-x", NoAction {}, Some("editor")),
        ];

        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        // Ensure `space` results in pending input on the workspace, but not editor
        let (result, pending) = keymap.bindings_for_input(
            &[Keystroke::parse("ctrl-x").unwrap()],
            &[KeyContext::parse("editor").unwrap()],
        );
        assert!(result.is_empty());
        assert!(!pending);
    }

    #[test]
    fn test_fail_to_disable() {
        // disabled at the wrong level
        let bindings = [
            KeyBinding::new("ctrl-x", ActionAlpha {}, Some("editor")),
            KeyBinding::new("ctrl-x", NoAction {}, Some("workspace")),
        ];

        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        // Ensure `space` results in pending input on the workspace, but not editor
        let (result, pending) = keymap.bindings_for_input(
            &[Keystroke::parse("ctrl-x").unwrap()],
            &[
                KeyContext::parse("workspace").unwrap(),
                KeyContext::parse("editor").unwrap(),
            ],
        );
        assert_eq!(result.len(), 1);
        assert!(!pending);
    }

    #[test]
    fn test_disable_deeper() {
        let bindings = [
            KeyBinding::new("ctrl-x", ActionAlpha {}, Some("workspace")),
            KeyBinding::new("ctrl-x", NoAction {}, Some("editor")),
        ];

        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        // Ensure `space` results in pending input on the workspace, but not editor
        let (result, pending) = keymap.bindings_for_input(
            &[Keystroke::parse("ctrl-x").unwrap()],
            &[
                KeyContext::parse("workspace").unwrap(),
                KeyContext::parse("editor").unwrap(),
            ],
        );
        assert_eq!(result.len(), 0);
        assert!(!pending);
    }

    #[test]
    fn test_pending_match_enabled() {
        let bindings = [
            KeyBinding::new("ctrl-x", ActionBeta, Some("vim_mode == normal")),
            KeyBinding::new("ctrl-x 0", ActionAlpha, Some("Workspace")),
        ];
        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        let matched = keymap.bindings_for_input(
            &[Keystroke::parse("ctrl-x")].map(Result::unwrap),
            &[
                KeyContext::parse("Workspace"),
                KeyContext::parse("Pane"),
                KeyContext::parse("Editor vim_mode=normal"),
            ]
            .map(Result::unwrap),
        );
        assert_eq!(matched.0.len(), 1);
        assert!(matched.0[0].action.partial_eq(&ActionBeta));
        assert!(matched.1);
    }

    #[test]
    fn test_pending_match_enabled_extended() {
        let bindings = [
            KeyBinding::new("ctrl-x", ActionBeta, Some("vim_mode == normal")),
            KeyBinding::new("ctrl-x 0", NoAction, Some("Workspace")),
        ];
        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        let matched = keymap.bindings_for_input(
            &[Keystroke::parse("ctrl-x")].map(Result::unwrap),
            &[
                KeyContext::parse("Workspace"),
                KeyContext::parse("Pane"),
                KeyContext::parse("Editor vim_mode=normal"),
            ]
            .map(Result::unwrap),
        );
        assert_eq!(matched.0.len(), 1);
        assert!(matched.0[0].action.partial_eq(&ActionBeta));
        assert!(!matched.1);
        let bindings = [
            KeyBinding::new("ctrl-x", ActionBeta, Some("Workspace")),
            KeyBinding::new("ctrl-x 0", NoAction, Some("vim_mode == normal")),
        ];
        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        let matched = keymap.bindings_for_input(
            &[Keystroke::parse("ctrl-x")].map(Result::unwrap),
            &[
                KeyContext::parse("Workspace"),
                KeyContext::parse("Pane"),
                KeyContext::parse("Editor vim_mode=normal"),
            ]
            .map(Result::unwrap),
        );
        assert_eq!(matched.0.len(), 1);
        assert!(matched.0[0].action.partial_eq(&ActionBeta));
        assert!(!matched.1);
    }

    #[test]
    fn test_overriding_prefix() {
        let bindings = [
            KeyBinding::new("ctrl-x 0", ActionAlpha, Some("Workspace")),
            KeyBinding::new("ctrl-x", ActionBeta, Some("vim_mode == normal")),
        ];
        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        let matched = keymap.bindings_for_input(
            &[Keystroke::parse("ctrl-x")].map(Result::unwrap),
            &[
                KeyContext::parse("Workspace"),
                KeyContext::parse("Pane"),
                KeyContext::parse("Editor vim_mode=normal"),
            ]
            .map(Result::unwrap),
        );
        assert_eq!(matched.0.len(), 1);
        assert!(matched.0[0].action.partial_eq(&ActionBeta));
        assert!(!matched.1);
    }

    #[test]
    fn test_context_precedence_with_same_source() {
        // Test case: User has both Workspace and Editor bindings for the same key
        // Editor binding should take precedence over Workspace binding
        let bindings = [
            KeyBinding::new("cmd-r", ActionAlpha {}, Some("Workspace")),
            KeyBinding::new("cmd-r", ActionBeta {}, Some("Editor")),
        ];

        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        // Test with context stack: [Workspace, Editor] (Editor is deeper)
        let (result, _) = keymap.bindings_for_input(
            &[Keystroke::parse("cmd-r").unwrap()],
            &[
                KeyContext::parse("Workspace").unwrap(),
                KeyContext::parse("Editor").unwrap(),
            ],
        );

        // Both bindings should be returned, but Editor binding should be first (highest precedence)
        assert_eq!(result.len(), 2);
        assert!(result[0].action.partial_eq(&ActionBeta {})); // Editor binding first
        assert!(result[1].action.partial_eq(&ActionAlpha {})); // Workspace binding second
    }

    #[test]
    fn test_bindings_for_action() {
        let bindings = [
            KeyBinding::new("ctrl-a", ActionAlpha {}, Some("pane")),
            KeyBinding::new("ctrl-b", ActionBeta {}, Some("editor && mode == full")),
            KeyBinding::new("ctrl-c", ActionGamma {}, Some("workspace")),
            KeyBinding::new("ctrl-a", NoAction {}, Some("pane && active")),
            KeyBinding::new("ctrl-b", NoAction {}, Some("editor")),
        ];

        let mut keymap = Keymap::default();
        keymap.add_bindings(bindings);

        assert_bindings(&keymap, &ActionAlpha {}, &["ctrl-a"]);
        assert_bindings(&keymap, &ActionBeta {}, &[]);
        assert_bindings(&keymap, &ActionGamma {}, &["ctrl-c"]);

        #[track_caller]
        fn assert_bindings(keymap: &Keymap, action: &dyn Action, expected: &[&str]) {
            let actual = keymap
                .bindings_for_action(action)
                .map(|binding| binding.keystrokes[0].inner().unparse())
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{:?}", action);
        }
    }

    #[test]
    fn test_no_action_source_matches_display_and_dispatch() {
        for meta in [
            Some(KeyBindingMetaIndex(2)),
            Some(KeyBindingMetaIndex::USER),
            None,
        ] {
            for (action_context, null_context, context) in [
                (None, None, "editor"),
                (Some("editor"), Some("editor"), "editor"),
                (Some("editor"), None, "editor"),
                (
                    Some("editor && mode == full"),
                    Some("editor"),
                    "editor mode=full",
                ),
            ] {
                let mut null_binding = KeyBinding::new("ctrl-a", NoAction, null_context);
                null_binding.meta = meta;
                let keymap = Keymap::new(vec![
                    KeyBinding::new("ctrl-a", ActionAlpha, action_context),
                    null_binding,
                ]);
                let expected_count = usize::from(meta.is_some_and(|meta| !meta.is_user()));
                let (dispatched, pending) = keymap.bindings_for_input(
                    &[Keystroke::parse("ctrl-a").unwrap()],
                    &[KeyContext::parse(context).unwrap()],
                );

                assert!(!pending);
                assert_eq!(dispatched.len(), expected_count, "source: {meta:?}");
                assert_eq!(
                    keymap.bindings_for_action(&ActionAlpha).count(),
                    dispatched.len(),
                    "display and dispatch disagree for source {meta:?}, action context {action_context:?}, null context {null_context:?}",
                );
            }
        }
    }

    #[test]
    fn test_no_action_source_matches_chord_pending_and_dispatch() {
        for (meta, expected_count) in [
            (Some(KeyBindingMetaIndex(2)), 1),
            (Some(KeyBindingMetaIndex::USER), 0),
            (None, 0),
        ] {
            let mut null_binding = KeyBinding::new("ctrl-k a", NoAction, Some("editor"));
            null_binding.meta = meta;
            let keymap = Keymap::new(vec![
                KeyBinding::new("ctrl-k a", ActionAlpha, Some("editor")),
                null_binding,
            ]);
            let input = [
                Keystroke::parse("ctrl-k").unwrap(),
                Keystroke::parse("a").unwrap(),
            ];
            let contexts = [KeyContext::parse("editor").unwrap()];
            let (prefix_matches, pending) = keymap.bindings_for_input(&input[..1], &contexts);
            let (dispatched, full_pending) = keymap.bindings_for_input(&input, &contexts);

            assert!(prefix_matches.is_empty());
            assert!(!full_pending);
            assert_eq!(dispatched.len(), expected_count, "source: {meta:?}");
            assert_eq!(pending, !dispatched.is_empty(), "source: {meta:?}");
            assert_eq!(
                keymap.bindings_for_action(&ActionAlpha).count(),
                expected_count
            );
            assert_eq!(
                keymap
                    .possible_next_bindings_for_input(&input[..1], &contexts)
                    .len(),
                expected_count,
            );
        }
    }

    #[test]
    fn chord_pending_respects_context_depth_before_registration_order() {
        let contexts = [
            KeyContext::parse("Workspace").unwrap(),
            KeyContext::parse("Editor").unwrap(),
        ];
        let input = ["ctrl-k", "a", "b"].map(|key| Keystroke::parse(key).unwrap());
        for (action_context, null_context, expected) in [
            ("Editor", "Workspace", true),
            ("Workspace", "Editor", false),
        ] {
            for reverse_order in [false, true] {
                for meta in [
                    None,
                    Some(KeyBindingMetaIndex::USER),
                    Some(KeyBindingMetaIndex(2)),
                ] {
                    let mut null_binding =
                        KeyBinding::new("ctrl-k a b", NoAction, Some(null_context));
                    null_binding.meta = meta;
                    let mut bindings = vec![
                        KeyBinding::new("ctrl-k a b", ActionAlpha, Some(action_context)),
                        null_binding,
                    ];
                    if reverse_order {
                        bindings.reverse();
                    }
                    let keymap = Keymap::new(bindings);
                    let (dispatched, _) = keymap.bindings_for_input(&input, &contexts);
                    let enabled = expected || meta.is_some_and(|meta| !meta.is_user());
                    assert_eq!(!dispatched.is_empty(), enabled);
                    for prefix_len in [1, 2] {
                        let (matched, pending) =
                            keymap.bindings_for_input(&input[..prefix_len], &contexts);
                        assert!(matched.is_empty());
                        assert_eq!(
                            pending, enabled,
                            "action={action_context}, null={null_context}, reverse={reverse_order}, source={meta:?}, prefix={prefix_len}"
                        );
                        assert_eq!(
                            !keymap
                                .possible_next_bindings_for_input(&input[..prefix_len], &contexts)
                                .is_empty(),
                            enabled
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn chord_pending_uses_the_typed_prefix_for_equivalent_keys() {
        let input = [Keystroke::parse("alt-s->ß").unwrap()];
        let keymap = Keymap::new(vec![
            KeyBinding::new("ß a", ActionAlpha, None),
            KeyBinding::new("alt-s a", NoAction, None),
        ]);
        assert!(!keymap.bindings_for_input(&input, &[]).1);
        assert!(
            keymap
                .possible_next_bindings_for_input(&input, &[])
                .is_empty()
        );
    }

    #[test]
    fn test_contextual_user_no_action_preserves_other_contexts() {
        for meta in [Some(KeyBindingMetaIndex::USER), None] {
            let mut null_binding =
                KeyBinding::new("ctrl-a", NoAction, Some("editor && mode == full"));
            null_binding.meta = meta;
            let keymap = Keymap::new(vec![
                KeyBinding::new("ctrl-a", ActionAlpha, Some("editor")),
                null_binding,
            ]);

            assert_eq!(keymap.bindings_for_action(&ActionAlpha).count(), 1);
            for (context, expected_count) in [("editor", 1), ("editor mode=full", 0)] {
                let (dispatched, pending) = keymap.bindings_for_input(
                    &[Keystroke::parse("ctrl-a").unwrap()],
                    &[KeyContext::parse(context).unwrap()],
                );
                assert!(!pending);
                assert_eq!(
                    dispatched.len(),
                    expected_count,
                    "source: {meta:?}, context: {context}"
                );
            }
        }
    }

    #[test]
    fn test_source_precedence_sorting() {
        // KeybindSource precedence: User (0) > Vim (1) > Base (2) > Default (3)
        // Test that user keymaps take precedence over default keymaps at the same context depth
        let mut keymap = Keymap::default();

        // Add a default keymap binding first
        let mut default_binding = KeyBinding::new("cmd-r", ActionAlpha {}, Some("Editor"));
        default_binding.set_meta(KeyBindingMetaIndex(3)); // Default source
        keymap.add_bindings([default_binding]);

        // Add a user keymap binding
        let mut user_binding = KeyBinding::new("cmd-r", ActionBeta {}, Some("Editor"));
        user_binding.set_meta(KeyBindingMetaIndex(0)); // User source
        keymap.add_bindings([user_binding]);

        // Test with Editor context stack
        let (result, _) = keymap.bindings_for_input(
            &[Keystroke::parse("cmd-r").unwrap()],
            &[KeyContext::parse("Editor").unwrap()],
        );

        // User binding should take precedence over default binding
        assert_eq!(result.len(), 2);
        assert!(result[0].action.partial_eq(&ActionBeta {}));
        assert!(result[1].action.partial_eq(&ActionAlpha {}));
    }

    #[test]
    fn test_possible_next_bindings_exclude_null_bindings() {
        let keymap = Keymap::new(vec![
            KeyBinding::new("ctrl-k a", ActionAlpha {}, Some("Editor")),
            KeyBinding::new("ctrl-k b", ActionBeta {}, Some("Editor")),
            KeyBinding::new("ctrl-k a", NoAction {}, Some("Editor")),
        ]);

        let bindings = keymap.possible_next_bindings_for_input(
            &[Keystroke::parse("ctrl-k").unwrap()],
            &[KeyContext::parse("Editor").unwrap()],
        );
        assert_eq!(
            describe_bindings(&bindings),
            vec![("ctrl-k b".to_string(), "test_only::ActionBeta")]
        );
    }

    #[test]
    fn test_possible_next_bindings_keep_user_binding_over_base_keymap_null() {
        const USER: KeyBindingMetaIndex = KeyBindingMetaIndex(0);
        const BASE: KeyBindingMetaIndex = KeyBindingMetaIndex(2);
        const DEFAULT: KeyBindingMetaIndex = KeyBindingMetaIndex(3);

        let mut keymap = Keymap::default();
        keymap.add_bindings([
            KeyBinding::new("ctrl-k a", ActionAlpha {}, Some("Editor")).with_meta(DEFAULT),
            KeyBinding::new("ctrl-k a", NoAction {}, Some("Editor")).with_meta(BASE),
            KeyBinding::new("ctrl-k a", ActionBeta {}, Some("Editor")).with_meta(USER),
        ]);

        let context = [KeyContext::parse("Editor").unwrap()];
        let bindings = keymap
            .possible_next_bindings_for_input(&[Keystroke::parse("ctrl-k").unwrap()], &context);
        // A base-keymap null (meta != 0) does not suppress bindings. Dispatch still
        // returns the user binding and the default binding, with the user binding first.
        let expected = vec![
            ("ctrl-k a".to_string(), "test_only::ActionBeta"),
            ("ctrl-k a".to_string(), "test_only::ActionAlpha"),
        ];
        assert_eq!(describe_bindings(&bindings), expected);

        let (dispatched, _) = keymap.bindings_for_input(
            &[
                Keystroke::parse("ctrl-k").unwrap(),
                Keystroke::parse("a").unwrap(),
            ],
            &context,
        );
        assert_eq!(describe_bindings(&dispatched), expected);
    }

    #[test]
    fn test_possible_next_bindings_apply_disabling_by_context() {
        let keymap = Keymap::new(vec![
            KeyBinding::new("ctrl-k a", ActionAlpha {}, Some("Workspace")),
            KeyBinding::new("ctrl-k a", ActionBeta {}, Some("Editor")),
            KeyBinding::new("ctrl-k b", ActionGamma {}, Some("Editor")),
            // NoAction is the local disable action. In Editor it removes `ctrl-k a`
            // entirely, including the Workspace binding. `ctrl-k b` is a different chord.
            KeyBinding::new("ctrl-k a", NoAction {}, Some("Editor")),
            // Doesn't apply because `Terminal` isn't in the context stack.
            KeyBinding::new("ctrl-k b", NoAction {}, Some("Terminal")),
        ]);

        let bindings = keymap.possible_next_bindings_for_input(
            &[Keystroke::parse("ctrl-k").unwrap()],
            &[
                KeyContext::parse("Workspace").unwrap(),
                KeyContext::parse("Editor").unwrap(),
            ],
        );
        assert_eq!(
            describe_bindings(&bindings),
            vec![("ctrl-k b".to_string(), "test_only::ActionGamma")]
        );
    }

    #[test]
    fn test_possible_next_bindings_exclude_longer_chord_disabled_in_deeper_context() {
        let keymap = Keymap::new(vec![
            KeyBinding::new("space w w", ActionAlpha {}, Some("Workspace")),
            KeyBinding::new("space w x", ActionBeta {}, Some("Workspace")),
            KeyBinding::new("space w w", NoAction {}, Some("AgentPanel")),
        ]);
        let context_stack = [
            KeyContext::parse("Workspace").unwrap(),
            KeyContext::parse("AgentPanel").unwrap(),
        ];

        for input in ["space", "space w"] {
            let input = input
                .split(' ')
                .map(|keystroke| Keystroke::parse(keystroke).unwrap())
                .collect::<Vec<_>>();
            let bindings = keymap.possible_next_bindings_for_input(&input, &context_stack);
            assert_eq!(
                describe_bindings(&bindings),
                vec![("space w x".to_string(), "test_only::ActionBeta")]
            );
        }
    }

    #[test]
    fn test_possible_next_bindings_exclude_disabled_bindings_with_typed_input() {
        // Option-S reports both its physical key and the character it produced, so typed input
        // matches bindings written with either.
        let typed_input = [Keystroke::parse("alt-s->ß").unwrap()];
        let keymap = Keymap::new(vec![
            KeyBinding::new("ß a", ActionAlpha {}, None),
            KeyBinding::new("alt-s a", NoAction {}, None),
        ]);
        assert!(
            keymap
                .possible_next_bindings_for_input(&typed_input, &[])
                .is_empty()
        );

        // Cross-check against dispatch of the same typed input.
        let typed_then_a = [
            Keystroke::parse("alt-s->ß").unwrap(),
            Keystroke::parse("a").unwrap(),
        ];
        let (dispatched, _) = keymap.bindings_for_input(&typed_then_a, &[]);
        assert!(dispatched.is_empty());
    }

    fn describe_bindings(bindings: &[KeyBinding]) -> Vec<(String, &'static str)> {
        bindings
            .iter()
            .map(|binding| {
                (
                    binding
                        .keystrokes()
                        .iter()
                        .map(|keystroke| keystroke.inner().unparse())
                        .collect::<Vec<_>>()
                        .join(" "),
                    binding.action().name(),
                )
            })
            .collect()
    }
}
