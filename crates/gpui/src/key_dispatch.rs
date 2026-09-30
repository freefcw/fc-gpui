//! KeyDispatch is where GPUI deals with binding actions to key events.
//!
//! The key pieces to making a key binding work are to define an action,
//! implement a method that takes that action as a type parameter,
//! and then to register the action during render on a focused node
//! with a keymap context:
//!
//! ```ignore
//! actions!(editor,[Undo, Redo]);
//!
//! impl Editor {
//!   fn undo(&mut self, _: &Undo, _window: &mut Window, _cx: &mut Context<Self>) { ... }
//!   fn redo(&mut self, _: &Redo, _window: &mut Window, _cx: &mut Context<Self>) { ... }
//! }
//!
//! impl Render for Editor {
//!   fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
//!     div()
//!       .track_focus(&self.focus_handle(cx))
//!       .key_context("Editor")
//!       .on_action(cx.listener(Editor::undo))
//!       .on_action(cx.listener(Editor::redo))
//!     ...
//!    }
//! }
//!```
//!
//! The keybindings themselves are managed independently by calling cx.bind_keys().
//! (Though mostly when developing Zed itself, you just need to add a new line to
//!  assets/keymaps/default-{platform}.json).
//!
//! ```ignore
//! cx.bind_keys([
//!   KeyBinding::new("cmd-z", Editor::undo, Some("Editor")),
//!   KeyBinding::new("cmd-shift-z", Editor::redo, Some("Editor")),
//! ])
//! ```
//!
//! With all of this in place, GPUI will ensure that if you have an Editor that contains
//! the focus, hitting cmd-z will Undo.
//!
//! In real apps, it is a little more complicated than this, because typically you have
//! several nested views that each register keyboard handlers. In this case action matching
//! bubbles up from the bottom. For example in Zed, the Workspace is the top-level view, which contains Pane's, which contain Editors. If there are conflicting keybindings defined
//! then the Editor's bindings take precedence over the Pane's bindings, which take precedence over the Workspace.
//!
//! In GPUI, keybindings are not limited to just single keystrokes, you can define
//! sequences by separating the keys with a space:
//!
//!  KeyBinding::new("cmd-k left", pane::SplitLeft, Some("Pane"))

use crate::{
    Action, ActionRegistry, App, DispatchPhase, EntityId, FocusId, KeyBinding, KeyContext, Keymap,
    Keystroke, ModifiersChangedEvent, Window,
};
use collections::FxHashMap;
use smallvec::SmallVec;
use std::{
    any::{Any, TypeId},
    cell::RefCell,
    mem,
    ops::Range,
    rc::Rc,
};

/// ID of a node within `DispatchTree`. Note that these are **not** stable between frames, and so a
/// `DispatchNodeId` should only be used with the `DispatchTree` that provided it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) struct DispatchNodeId(usize);

pub(crate) struct DispatchTree {
    node_stack: Vec<DispatchNodeId>,
    pub(crate) context_stack: Vec<KeyContext>,
    view_stack: Vec<EntityId>,
    nodes: Vec<DispatchNode>,
    focusable_node_ids: FxHashMap<FocusId, DispatchNodeId>,
    view_node_ids: FxHashMap<EntityId, DispatchNodeId>,
    keymap: Rc<RefCell<Keymap>>,
    action_registry: Rc<ActionRegistry>,
}

#[derive(Default)]
pub(crate) struct DispatchNode {
    pub key_listeners: Vec<KeyListener>,
    pub action_listeners: Vec<DispatchActionListener>,
    pub modifiers_changed_listeners: Vec<ModifiersChangedListener>,
    pub context: Option<KeyContext>,
    pub focus_id: Option<FocusId>,
    view_id: Option<EntityId>,
    parent: Option<DispatchNodeId>,
}

pub(crate) struct ReusedSubtree {
    old_range: Range<usize>,
    new_range: Range<usize>,
    contains_focus: bool,
}

impl ReusedSubtree {
    pub fn refresh_node_id(&self, node_id: DispatchNodeId) -> DispatchNodeId {
        debug_assert!(
            self.old_range.contains(&node_id.0),
            "node {} was not part of the reused subtree {:?}",
            node_id.0,
            self.old_range
        );
        DispatchNodeId((node_id.0 - self.old_range.start) + self.new_range.start)
    }

    pub fn contains_focus(&self) -> bool {
        self.contains_focus
    }
}

#[derive(Default, Debug)]
pub(crate) struct Replay {
    pub(crate) keystroke: Keystroke,
    pub(crate) bindings: SmallVec<[KeyBinding; 1]>,
}

#[derive(Default, Debug)]
pub(crate) struct DispatchResult {
    pub(crate) pending: SmallVec<[Keystroke; 1]>,
    pub(crate) bindings: SmallVec<[KeyBinding; 1]>,
    pub(crate) to_replay: SmallVec<[Replay; 1]>,
    pub(crate) context_stack: Vec<KeyContext>,
}

type KeyListener = Rc<dyn Fn(&dyn Any, DispatchPhase, &mut Window, &mut App)>;
type ModifiersChangedListener = Rc<dyn Fn(&ModifiersChangedEvent, &mut Window, &mut App)>;

#[derive(Clone)]
pub(crate) struct DispatchActionListener {
    pub(crate) action_type: TypeId,
    pub(crate) listener: Rc<dyn Fn(&dyn Any, DispatchPhase, &mut Window, &mut App)>,
}

impl DispatchTree {
    pub fn new(keymap: Rc<RefCell<Keymap>>, action_registry: Rc<ActionRegistry>) -> Self {
        Self {
            node_stack: Vec::new(),
            context_stack: Vec::new(),
            view_stack: Vec::new(),
            nodes: Vec::new(),
            focusable_node_ids: FxHashMap::default(),
            view_node_ids: FxHashMap::default(),
            keymap,
            action_registry,
        }
    }

    pub fn clear(&mut self) {
        self.node_stack.clear();
        self.context_stack.clear();
        self.view_stack.clear();
        self.nodes.clear();
        self.focusable_node_ids.clear();
        self.view_node_ids.clear();
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn push_node(&mut self) -> DispatchNodeId {
        let parent = self.node_stack.last().copied();
        let node_id = DispatchNodeId(self.nodes.len());

        self.nodes.push(DispatchNode {
            parent,
            ..Default::default()
        });
        self.node_stack.push(node_id);
        node_id
    }

    pub fn set_active_node(&mut self, node_id: DispatchNodeId) {
        let next_node_parent = self.nodes[node_id.0].parent;
        while self.node_stack.last().copied() != next_node_parent && !self.node_stack.is_empty() {
            self.pop_node();
        }

        if self.node_stack.last().copied() == next_node_parent {
            self.node_stack.push(node_id);
            let active_node = &self.nodes[node_id.0];
            if let Some(view_id) = active_node.view_id {
                self.view_stack.push(view_id)
            }
            if let Some(context) = active_node.context.clone() {
                self.context_stack.push(context);
            }
        } else {
            debug_assert_eq!(self.node_stack.len(), 0);

            let mut current_node_id = Some(node_id);
            while let Some(node_id) = current_node_id {
                let node = &self.nodes[node_id.0];
                if let Some(context) = node.context.clone() {
                    self.context_stack.push(context);
                }
                if let Some(view_id) = node.view_id {
                    self.view_stack.push(view_id);
                }
                self.node_stack.push(node_id);
                current_node_id = node.parent;
            }

            self.context_stack.reverse();
            self.view_stack.reverse();
            self.node_stack.reverse();
        }
    }

    pub fn set_key_context(&mut self, context: KeyContext) {
        self.active_node().context = Some(context.clone());
        self.context_stack.push(context);
    }

    pub fn set_focus_id(&mut self, focus_id: FocusId) {
        let node_id = *self.node_stack.last().unwrap();
        self.nodes[node_id.0].focus_id = Some(focus_id);
        self.focusable_node_ids.insert(focus_id, node_id);
    }

    pub fn set_view_id(&mut self, view_id: EntityId) {
        if self.view_stack.last().copied() != Some(view_id) {
            let node_id = *self.node_stack.last().unwrap();
            self.nodes[node_id.0].view_id = Some(view_id);
            self.view_node_ids.insert(view_id, node_id);
            self.view_stack.push(view_id);
        }
    }

    pub fn pop_node(&mut self) {
        let node = &self.nodes[self.active_node_id().unwrap().0];
        if node.context.is_some() {
            self.context_stack.pop();
        }
        if node.view_id.is_some() {
            self.view_stack.pop();
        }
        self.node_stack.pop();
    }

    fn move_node(&mut self, source: &mut DispatchNode) {
        self.push_node();
        if let Some(context) = source.context.clone() {
            self.set_key_context(context);
        }
        if let Some(focus_id) = source.focus_id {
            self.set_focus_id(focus_id);
        }
        if let Some(view_id) = source.view_id {
            self.set_view_id(view_id);
        }

        let target = self.active_node();
        target.key_listeners = mem::take(&mut source.key_listeners);
        target.action_listeners = mem::take(&mut source.action_listeners);
        target.modifiers_changed_listeners = mem::take(&mut source.modifiers_changed_listeners);
    }

    pub fn reuse_subtree(
        &mut self,
        old_range: Range<usize>,
        source: &mut Self,
        focus: Option<FocusId>,
    ) -> ReusedSubtree {
        let new_range = self.nodes.len()..self.nodes.len() + old_range.len();

        let mut contains_focus = false;
        let mut source_stack = vec![];
        for (source_node_id, source_node) in source
            .nodes
            .iter_mut()
            .enumerate()
            .skip(old_range.start)
            .take(old_range.len())
        {
            let source_node_id = DispatchNodeId(source_node_id);
            while let Some(source_ancestor) = source_stack.last() {
                if source_node.parent == Some(*source_ancestor) {
                    break;
                } else {
                    source_stack.pop();
                    self.pop_node();
                }
            }

            source_stack.push(source_node_id);
            if source_node.focus_id.is_some() && source_node.focus_id == focus {
                contains_focus = true;
            }
            self.move_node(source_node);
        }

        while !source_stack.is_empty() {
            source_stack.pop();
            self.pop_node();
        }

        ReusedSubtree {
            old_range,
            new_range,
            contains_focus,
        }
    }

    pub fn truncate(&mut self, index: usize) {
        for node in &self.nodes[index..] {
            if let Some(focus_id) = node.focus_id {
                self.focusable_node_ids.remove(&focus_id);
            }

            if let Some(view_id) = node.view_id {
                self.view_node_ids.remove(&view_id);
            }
        }
        self.nodes.truncate(index);
    }

    pub fn on_key_event(&mut self, listener: KeyListener) {
        self.active_node().key_listeners.push(listener);
    }

    pub fn on_modifiers_changed(&mut self, listener: ModifiersChangedListener) {
        self.active_node()
            .modifiers_changed_listeners
            .push(listener);
    }

    pub fn on_action(
        &mut self,
        action_type: TypeId,
        listener: Rc<dyn Fn(&dyn Any, DispatchPhase, &mut Window, &mut App)>,
    ) {
        self.active_node()
            .action_listeners
            .push(DispatchActionListener {
                action_type,
                listener,
            });
    }

    pub fn focus_contains(&self, parent: FocusId, child: FocusId) -> bool {
        if parent == child {
            return true;
        }

        if let Some(parent_node_id) = self.focusable_node_ids.get(&parent) {
            let mut current_node_id = self.focusable_node_ids.get(&child).copied();
            while let Some(node_id) = current_node_id {
                if node_id == *parent_node_id {
                    return true;
                }
                current_node_id = self.nodes[node_id.0].parent;
            }
        }
        false
    }

    pub fn available_actions(&self, target: DispatchNodeId) -> Vec<Box<dyn Action>> {
        let mut actions = Vec::<Box<dyn Action>>::new();
        for node_id in self.dispatch_path(target) {
            let node = &self.nodes[node_id.0];
            for DispatchActionListener { action_type, .. } in &node.action_listeners {
                if let Err(ix) = actions.binary_search_by_key(action_type, |a| a.as_any().type_id())
                {
                    // Intentionally silence these errors without logging.
                    // If an action cannot be built by default, it's not available.
                    let action = self.action_registry.build_action_type(action_type).ok();
                    if let Some(action) = action {
                        actions.insert(ix, action);
                    }
                }
            }
        }
        actions
    }

    pub fn is_action_available(&self, action: &dyn Action, target: DispatchNodeId) -> bool {
        for node_id in self.dispatch_path(target) {
            let node = &self.nodes[node_id.0];
            if node
                .action_listeners
                .iter()
                .any(|listener| listener.action_type == action.as_any().type_id())
            {
                return true;
            }
        }
        false
    }

    /// Returns key bindings that invoke an action on the currently focused element. Bindings are
    /// returned in the order they were added. For display, the last binding should take precedence.
    ///
    /// Bindings are only included if they are the highest precedence match for their keystrokes, so
    /// shadowed bindings are not included.
    pub fn bindings_for_action(
        &self,
        action: &dyn Action,
        context_stack: &[KeyContext],
    ) -> Vec<KeyBinding> {
        // Ideally this would return a `DoubleEndedIterator` to avoid `highest_precedence_*`
        // methods, but this can't be done very cleanly since keymap must be borrowed.
        let keymap = self.keymap.borrow();
        keymap
            .bindings_for_action(action)
            .filter(|binding| {
                Self::binding_matches_predicate_and_not_shadowed(&keymap, binding, context_stack)
            })
            .cloned()
            .collect()
    }

    /// Returns the highest precedence binding for the given action and context stack. This is the
    /// same as the last result of `bindings_for_action`, but more efficient than getting all bindings.
    pub fn highest_precedence_binding_for_action(
        &self,
        action: &dyn Action,
        context_stack: &[KeyContext],
    ) -> Option<KeyBinding> {
        let keymap = self.keymap.borrow();
        keymap
            .bindings_for_action(action)
            .rev()
            .find(|binding| {
                Self::binding_matches_predicate_and_not_shadowed(&keymap, binding, context_stack)
            })
            .cloned()
    }

    fn binding_matches_predicate_and_not_shadowed(
        keymap: &Keymap,
        binding: &KeyBinding,
        context_stack: &[KeyContext],
    ) -> bool {
        let (bindings, _) = keymap.bindings_for_input(&binding.keystrokes, context_stack);
        if let Some(found) = bindings.iter().next() {
            found.action.partial_eq(binding.action.as_ref())
        } else {
            false
        }
    }

    fn bindings_for_input(
        &self,
        input: &[Keystroke],
        dispatch_path: &SmallVec<[DispatchNodeId; 32]>,
    ) -> (SmallVec<[KeyBinding; 1]>, bool, Vec<KeyContext>) {
        let context_stack: Vec<KeyContext> = dispatch_path
            .iter()
            .filter_map(|node_id| self.node(*node_id).context.clone())
            .collect();

        let (bindings, partial) = self
            .keymap
            .borrow()
            .bindings_for_input(input, &context_stack);
        (bindings, partial, context_stack)
    }

    /// dispatch_key processes the keystroke
    /// input should be set to the value of `pending` from the previous call to dispatch_key.
    /// This returns three instructions to the input handler:
    /// - bindings: any bindings to execute before processing this keystroke
    /// - pending: the new set of pending keystrokes to store
    /// - to_replay: any keystroke that had been pushed to pending, but are no-longer matched,
    ///   these should be replayed first.
    pub fn dispatch_key(
        &mut self,
        mut input: SmallVec<[Keystroke; 1]>,
        keystroke: Keystroke,
        dispatch_path: &SmallVec<[DispatchNodeId; 32]>,
    ) -> DispatchResult {
        input.push(keystroke.clone());
        let (bindings, pending, context_stack) = self.bindings_for_input(&input, dispatch_path);

        if pending {
            return DispatchResult {
                pending: input,
                context_stack,
                ..Default::default()
            };
        } else if !bindings.is_empty() {
            return DispatchResult {
                bindings,
                context_stack,
                ..Default::default()
            };
        } else if input.len() == 1 {
            return DispatchResult {
                context_stack,
                ..Default::default()
            };
        }
        input.pop();

        let (suffix, mut to_replay) = self.replay_prefix(input, dispatch_path);

        let mut result = self.dispatch_key(suffix, keystroke, dispatch_path);
        to_replay.extend(result.to_replay);
        result.to_replay = to_replay;
        result
    }

    /// If the user types a matching prefix of a binding and then waits for a timeout
    /// flush_dispatch() converts any previously pending input to replay events.
    pub fn flush_dispatch(
        &mut self,
        input: SmallVec<[Keystroke; 1]>,
        dispatch_path: &SmallVec<[DispatchNodeId; 32]>,
    ) -> SmallVec<[Replay; 1]> {
        let (suffix, mut to_replay) = self.replay_prefix(input, dispatch_path);

        if !suffix.is_empty() {
            to_replay.extend(self.flush_dispatch(suffix, dispatch_path))
        }

        to_replay
    }

    /// Converts the longest prefix of input to a replay event and returns the rest.
    fn replay_prefix(
        &self,
        mut input: SmallVec<[Keystroke; 1]>,
        dispatch_path: &SmallVec<[DispatchNodeId; 32]>,
    ) -> (SmallVec<[Keystroke; 1]>, SmallVec<[Replay; 1]>) {
        let mut to_replay: SmallVec<[Replay; 1]> = Default::default();
        for last in (0..input.len()).rev() {
            let (bindings, _, _) = self.bindings_for_input(&input[0..=last], dispatch_path);
            if !bindings.is_empty() {
                to_replay.push(Replay {
                    keystroke: input.drain(0..=last).next_back().unwrap(),
                    bindings,
                });
                break;
            }
        }
        if to_replay.is_empty() {
            to_replay.push(Replay {
                keystroke: input.remove(0),
                ..Default::default()
            });
        }
        (input, to_replay)
    }

    pub fn dispatch_path(&self, target: DispatchNodeId) -> SmallVec<[DispatchNodeId; 32]> {
        let mut dispatch_path: SmallVec<[DispatchNodeId; 32]> = SmallVec::new();
        let mut current_node_id = Some(target);
        while let Some(node_id) = current_node_id {
            dispatch_path.push(node_id);
            current_node_id = self.nodes.get(node_id.0).and_then(|node| node.parent);
        }
        dispatch_path.reverse(); // Reverse the path so it goes from the root to the focused node.
        dispatch_path
    }

    pub fn focus_path(&self, focus_id: FocusId) -> SmallVec<[FocusId; 8]> {
        let mut focus_path: SmallVec<[FocusId; 8]> = SmallVec::new();
        let mut current_node_id = self.focusable_node_ids.get(&focus_id).copied();
        while let Some(node_id) = current_node_id {
            let node = self.node(node_id);
            if let Some(focus_id) = node.focus_id {
                focus_path.push(focus_id);
            }
            current_node_id = node.parent;
        }
        focus_path.reverse(); // Reverse the path so it goes from the root to the focused node.
        focus_path
    }

    pub fn view_path(&self, view_id: EntityId) -> SmallVec<[EntityId; 8]> {
        let mut view_path: SmallVec<[EntityId; 8]> = SmallVec::new();
        let mut current_node_id = self.view_node_ids.get(&view_id).copied();
        while let Some(node_id) = current_node_id {
            let node = self.node(node_id);
            if let Some(view_id) = node.view_id {
                view_path.push(view_id);
            }
            current_node_id = node.parent;
        }
        view_path.reverse(); // Reverse the path so it goes from the root to the view node.
        view_path
    }

    pub fn node(&self, node_id: DispatchNodeId) -> &DispatchNode {
        &self.nodes[node_id.0]
    }

    fn active_node(&mut self) -> &mut DispatchNode {
        let active_node_id = self.active_node_id().unwrap();
        &mut self.nodes[active_node_id.0]
    }

    pub fn focusable_node_id(&self, target: FocusId) -> Option<DispatchNodeId> {
        self.focusable_node_ids.get(&target).copied()
    }

    pub fn root_node_id(&self) -> DispatchNodeId {
        debug_assert!(!self.nodes.is_empty());
        DispatchNodeId(0)
    }

    pub fn active_node_id(&self) -> Option<DispatchNodeId> {
        self.node_stack.last().copied()
    }
}

#[cfg(test)]
mod tests {
    use crate::{Element, ElementId, GlobalElementId, InspectorElementId, LayoutId, Style};
    use core::panic;
    use std::{
        cell::{Cell, RefCell},
        ops::Range,
        rc::Rc,
        time::Duration,
    };

    use crate::{
        Action, ActionRegistry, App, AppContext, Bounds, Context, DispatchEventResult,
        DispatchPhase, DispatchTree, Entity, FocusHandle, InputHandler, InputPreference,
        IntoElement, KeyBinding, KeyContext, KeyDownEvent, Keymap, Keystroke, Modifiers, Pixels,
        PlatformInput, Point, Render, Subscription, TestAppContext, UTF16Selection, Window,
    };

    /// Matches the pending-input timer in `Window::dispatch_key_event`.
    const PENDING_INPUT_TIMEOUT: Duration = Duration::from_secs(1);

    #[derive(PartialEq, Eq)]
    struct TestAction;

    impl Action for TestAction {
        fn name(&self) -> &'static str {
            "test::TestAction"
        }

        fn name_for_type() -> &'static str
        where
            Self: ::std::marker::Sized,
        {
            "test::TestAction"
        }

        fn partial_eq(&self, action: &dyn Action) -> bool {
            action.as_any().downcast_ref::<Self>() == Some(self)
        }

        fn boxed_clone(&self) -> std::boxed::Box<dyn Action> {
            Box::new(TestAction)
        }

        fn build(_value: serde_json::Value) -> anyhow::Result<Box<dyn Action>>
        where
            Self: Sized,
        {
            Ok(Box::new(TestAction))
        }
    }

    #[derive(PartialEq, Eq)]
    struct SecondaryTestAction;

    impl Action for SecondaryTestAction {
        fn name(&self) -> &'static str {
            "test::SecondaryTestAction"
        }

        fn name_for_type() -> &'static str
        where
            Self: ::std::marker::Sized,
        {
            "test::SecondaryTestAction"
        }

        fn partial_eq(&self, action: &dyn Action) -> bool {
            action.as_any().downcast_ref::<Self>() == Some(self)
        }

        fn boxed_clone(&self) -> std::boxed::Box<dyn Action> {
            Box::new(SecondaryTestAction)
        }

        fn build(_value: serde_json::Value) -> anyhow::Result<Box<dyn Action>>
        where
            Self: Sized,
        {
            Ok(Box::new(SecondaryTestAction))
        }
    }

    struct PendingInputTestView {
        focus_handle: FocusHandle,
        action_count: Rc<Cell<usize>>,
        secondary_action_count: Rc<Cell<usize>>,
    }

    impl Render for PendingInputTestView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            use crate::{InteractiveElement as _, Styled as _};
            let action_count = self.action_count.clone();
            let secondary_action_count = self.secondary_action_count.clone();
            crate::div()
                .key_context("Terminal")
                .track_focus(&self.focus_handle)
                .size_full()
                .on_action(move |_: &TestAction, _, _| {
                    action_count.set(action_count.get() + 1);
                })
                .on_action(move |_: &SecondaryTestAction, _, _| {
                    secondary_action_count.set(secondary_action_count.get() + 1);
                })
        }
    }

    #[derive(Clone)]
    struct PendingTextInputTestView {
        focus_handle: FocusHandle,
        text: Rc<RefCell<String>>,
        action_count: Rc<Cell<usize>>,
        accepts_text_input: Rc<Cell<bool>>,
    }

    impl PendingTextInputTestView {
        fn new(cx: &mut Context<Self>) -> Self {
            Self {
                focus_handle: cx.focus_handle(),
                text: Rc::default(),
                action_count: Rc::default(),
                accepts_text_input: Rc::new(Cell::new(true)),
            }
        }
    }

    impl Element for PendingTextInputTestView {
        type RequestLayoutState = ();
        type PrepaintState = ();

        fn id(&self) -> Option<ElementId> {
            Some("pending-text-input-test".into())
        }

        fn source_location(&self) -> Option<&'static panic::Location<'static>> {
            None
        }

        fn request_layout(
            &mut self,
            _: Option<&GlobalElementId>,
            _: Option<&InspectorElementId>,
            window: &mut Window,
            cx: &mut App,
        ) -> (LayoutId, Self::RequestLayoutState) {
            (window.request_layout(Style::default(), [], cx), ())
        }

        fn prepaint(
            &mut self,
            _: Option<&GlobalElementId>,
            _: Option<&InspectorElementId>,
            _: Bounds<Pixels>,
            _: &mut Self::RequestLayoutState,
            window: &mut Window,
            cx: &mut App,
        ) -> Self::PrepaintState {
            window.set_focus_handle(&self.focus_handle, cx);
        }

        fn paint(
            &mut self,
            _: Option<&GlobalElementId>,
            _: Option<&InspectorElementId>,
            _: Bounds<Pixels>,
            _: &mut Self::RequestLayoutState,
            _: &mut Self::PrepaintState,
            window: &mut Window,
            cx: &mut App,
        ) {
            let mut key_context = KeyContext::default();
            key_context.add("Terminal");
            window.set_key_context(key_context);
            window.handle_input(&self.focus_handle, self.clone(), cx);
            let action_count = self.action_count.clone();
            window.on_action(
                std::any::TypeId::of::<TestAction>(),
                move |_, phase, _, _| {
                    if phase == DispatchPhase::Bubble {
                        action_count.set(action_count.get() + 1);
                    }
                },
            );
        }
    }

    impl IntoElement for PendingTextInputTestView {
        type Element = Self;

        fn into_element(self) -> Self::Element {
            self
        }
    }

    impl InputHandler for PendingTextInputTestView {
        fn accepts_text_input(&mut self, _: &mut Window, _: &mut App) -> bool {
            self.accepts_text_input.get()
        }

        fn selected_text_range(
            &mut self,
            _: bool,
            _: &mut Window,
            _: &mut App,
        ) -> Option<UTF16Selection> {
            None
        }

        fn marked_text_range(&mut self, _: &mut Window, _: &mut App) -> Option<Range<usize>> {
            None
        }

        fn text_for_range(
            &mut self,
            _: Range<usize>,
            _: &mut Option<Range<usize>>,
            _: &mut Window,
            _: &mut App,
        ) -> Option<String> {
            None
        }

        fn replace_text_in_range(
            &mut self,
            replacement_range: Option<Range<usize>>,
            text: &str,
            _: &mut Window,
            _: &mut App,
        ) {
            if replacement_range.is_some() {
                unimplemented!()
            }
            self.text.borrow_mut().push_str(text)
        }

        fn replace_and_mark_text_in_range(
            &mut self,
            replacement_range: Option<Range<usize>>,
            new_text: &str,
            _: Option<Range<usize>>,
            _: &mut Window,
            _: &mut App,
        ) {
            if replacement_range.is_some() {
                unimplemented!()
            }
            self.text.borrow_mut().push_str(new_text)
        }

        fn unmark_text(&mut self, _: &mut Window, _: &mut App) {}

        fn bounds_for_range(
            &mut self,
            _: Range<usize>,
            _: &mut Window,
            _: &mut App,
        ) -> Option<Bounds<Pixels>> {
            None
        }

        fn character_index_for_point(
            &mut self,
            _: Point<Pixels>,
            _: &mut Window,
            _: &mut App,
        ) -> Option<usize> {
            None
        }
    }

    impl Render for PendingTextInputTestView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.clone()
        }
    }

    fn setup_pending_input_test(
        cx: &mut TestAppContext,
        bindings: impl IntoIterator<Item = KeyBinding>,
    ) -> (
        &mut crate::VisualTestContext,
        Rc<Cell<usize>>,
        Rc<Cell<usize>>,
    ) {
        cx.update(|cx| cx.bind_keys(bindings));

        let action_count = Rc::new(Cell::new(0));
        let secondary_action_count = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view(|_, cx| PendingInputTestView {
            focus_handle: cx.focus_handle(),
            action_count: action_count.clone(),
            secondary_action_count: secondary_action_count.clone(),
        });
        let focus_handle = cx.update(|_, cx| view.read(cx).focus_handle.clone());
        cx.update(|window, _| {
            window.focus(&focus_handle);
            window.activate_window();
        });

        (cx, action_count, secondary_action_count)
    }

    #[derive(Debug, PartialEq)]
    struct ObservedKeystroke {
        keystroke: Keystroke,
        action_name: Option<&'static str>,
        context_stack: Vec<KeyContext>,
    }

    fn capture_observed_keystrokes(
        cx: &mut crate::VisualTestContext,
    ) -> (Rc<RefCell<Vec<ObservedKeystroke>>>, Subscription) {
        let observed_keystrokes = Rc::new(RefCell::new(Vec::new()));
        let subscription = cx.update(|_, cx| {
            cx.observe_keystrokes({
                let observed_keystrokes = observed_keystrokes.clone();
                move |event, _, _| {
                    observed_keystrokes.borrow_mut().push(ObservedKeystroke {
                        keystroke: event.keystroke.clone(),
                        action_name: event.action.as_ref().map(|action| action.name()),
                        context_stack: event.context_stack.clone(),
                    });
                }
            })
        });
        (observed_keystrokes, subscription)
    }

    fn setup_altgr_test(
        cx: &mut TestAppContext,
    ) -> (
        Entity<PendingTextInputTestView>,
        &mut crate::VisualTestContext,
    ) {
        let (test, cx) = cx.add_window_view(|_, cx| PendingTextInputTestView::new(cx));
        cx.update(|window, cx| {
            let focus_handle = test.read(cx).focus_handle.clone();
            window.focus(&focus_handle);
            window.activate_window();
            cx.bind_keys([KeyBinding::new("ctrl-alt-a", TestAction, Some("Terminal"))]);
        });
        (test, cx)
    }

    fn dispatch_altgr_key_down(
        cx: &mut crate::VisualTestContext,
        prefer_character_input: bool,
    ) -> DispatchEventResult {
        cx.update(|window, cx| {
            window.dispatch_event(
                PlatformInput::KeyDown(KeyDownEvent {
                    keystroke: Keystroke {
                        key_char: Some("ą".into()),
                        ..Keystroke::parse("ctrl-alt-a").expect("valid keystroke")
                    },
                    is_held: false,
                    prefer_character_input,
                }),
                cx,
            )
        })
    }

    fn simulate_modifier_tap(cx: &mut crate::VisualTestContext, modifiers: Modifiers) {
        cx.simulate_modifiers_change(modifiers);
        cx.simulate_modifiers_change(Modifiers::none());
    }

    fn terminal_context() -> KeyContext {
        KeyContext::parse("Terminal").expect("valid key context")
    }

    fn function_modifier() -> Modifiers {
        Modifiers {
            function: true,
            ..Modifiers::none()
        }
    }

    #[test]
    fn test_keybinding_for_action_bounds() {
        let keymap = Keymap::new(vec![KeyBinding::new(
            "cmd-n",
            TestAction,
            Some("ProjectPanel"),
        )]);

        let mut registry = ActionRegistry::default();

        registry.load_action::<TestAction>();

        let keymap = Rc::new(RefCell::new(keymap));

        let tree = DispatchTree::new(keymap, Rc::new(registry));

        let contexts = vec![
            KeyContext::parse("Workspace").unwrap(),
            KeyContext::parse("ProjectPanel").unwrap(),
        ];

        let keybinding = tree.bindings_for_action(&TestAction, &contexts);

        assert!(keybinding[0].action.partial_eq(&TestAction))
    }

    #[crate::test]
    fn test_input_handler_pending(cx: &mut TestAppContext) {
        #[derive(Clone)]
        struct CustomElement {
            focus_handle: FocusHandle,
            text: Rc<RefCell<String>>,
        }
        impl CustomElement {
            fn new(cx: &mut Context<Self>) -> Self {
                Self {
                    focus_handle: cx.focus_handle(),
                    text: Rc::default(),
                }
            }
        }
        impl Element for CustomElement {
            type RequestLayoutState = ();

            type PrepaintState = ();

            fn id(&self) -> Option<ElementId> {
                Some("custom".into())
            }
            fn source_location(&self) -> Option<&'static panic::Location<'static>> {
                None
            }
            fn request_layout(
                &mut self,
                _: Option<&GlobalElementId>,
                _: Option<&InspectorElementId>,
                window: &mut Window,
                cx: &mut App,
            ) -> (LayoutId, Self::RequestLayoutState) {
                (window.request_layout(Style::default(), [], cx), ())
            }
            fn prepaint(
                &mut self,
                _: Option<&GlobalElementId>,
                _: Option<&InspectorElementId>,
                _: Bounds<Pixels>,
                _: &mut Self::RequestLayoutState,
                window: &mut Window,
                cx: &mut App,
            ) -> Self::PrepaintState {
                window.set_focus_handle(&self.focus_handle, cx);
            }
            fn paint(
                &mut self,
                _: Option<&GlobalElementId>,
                _: Option<&InspectorElementId>,
                _: Bounds<Pixels>,
                _: &mut Self::RequestLayoutState,
                _: &mut Self::PrepaintState,
                window: &mut Window,
                cx: &mut App,
            ) {
                let mut key_context = KeyContext::default();
                key_context.add("Terminal");
                window.set_key_context(key_context);
                window.handle_input(&self.focus_handle, self.clone(), cx);
                window.on_action(std::any::TypeId::of::<TestAction>(), |_, _, _, _| {});
            }
        }
        impl IntoElement for CustomElement {
            type Element = Self;

            fn into_element(self) -> Self::Element {
                self
            }
        }

        impl InputHandler for CustomElement {
            fn selected_text_range(
                &mut self,
                _: bool,
                _: &mut Window,
                _: &mut App,
            ) -> Option<UTF16Selection> {
                None
            }

            fn marked_text_range(&mut self, _: &mut Window, _: &mut App) -> Option<Range<usize>> {
                None
            }

            fn text_for_range(
                &mut self,
                _: Range<usize>,
                _: &mut Option<Range<usize>>,
                _: &mut Window,
                _: &mut App,
            ) -> Option<String> {
                None
            }

            fn replace_text_in_range(
                &mut self,
                replacement_range: Option<Range<usize>>,
                text: &str,
                _: &mut Window,
                _: &mut App,
            ) {
                if replacement_range.is_some() {
                    unimplemented!()
                }
                self.text.borrow_mut().push_str(text)
            }

            fn replace_and_mark_text_in_range(
                &mut self,
                replacement_range: Option<Range<usize>>,
                new_text: &str,
                _: Option<Range<usize>>,
                _: &mut Window,
                _: &mut App,
            ) {
                if replacement_range.is_some() {
                    unimplemented!()
                }
                self.text.borrow_mut().push_str(new_text)
            }

            fn unmark_text(&mut self, _: &mut Window, _: &mut App) {}

            fn bounds_for_range(
                &mut self,
                _: Range<usize>,
                _: &mut Window,
                _: &mut App,
            ) -> Option<Bounds<Pixels>> {
                None
            }

            fn character_index_for_point(
                &mut self,
                _: Point<Pixels>,
                _: &mut Window,
                _: &mut App,
            ) -> Option<usize> {
                None
            }
        }
        impl Render for CustomElement {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                self.clone()
            }
        }

        cx.update(|cx| {
            cx.bind_keys([KeyBinding::new("ctrl-b", TestAction, Some("Terminal"))]);
            cx.bind_keys([KeyBinding::new("ctrl-b h", TestAction, Some("Terminal"))]);
        });
        let (test, cx) = cx.add_window_view(|_, cx| CustomElement::new(cx));
        cx.update(|window, cx| {
            window.focus(&test.read(cx).focus_handle);
            window.activate_window();
        });
        cx.simulate_keystrokes("ctrl-b [");
        test.update(cx, |test, _| assert_eq!(test.text.borrow().as_str(), "["))
    }

    #[crate::test]
    fn test_pending_input_cleared_on_blur(cx: &mut TestAppContext) {
        #[derive(Clone)]
        struct CustomElement {
            focus_handle: FocusHandle,
            text: Rc<RefCell<String>>,
        }
        impl CustomElement {
            fn new(cx: &mut Context<Self>) -> Self {
                Self {
                    focus_handle: cx.focus_handle(),
                    text: Rc::default(),
                }
            }
        }
        impl Element for CustomElement {
            type RequestLayoutState = ();

            type PrepaintState = ();

            fn id(&self) -> Option<ElementId> {
                Some("custom".into())
            }
            fn source_location(&self) -> Option<&'static panic::Location<'static>> {
                None
            }
            fn request_layout(
                &mut self,
                _: Option<&GlobalElementId>,
                _: Option<&InspectorElementId>,
                window: &mut Window,
                cx: &mut App,
            ) -> (LayoutId, Self::RequestLayoutState) {
                (window.request_layout(Style::default(), [], cx), ())
            }
            fn prepaint(
                &mut self,
                _: Option<&GlobalElementId>,
                _: Option<&InspectorElementId>,
                _: Bounds<Pixels>,
                _: &mut Self::RequestLayoutState,
                window: &mut Window,
                cx: &mut App,
            ) -> Self::PrepaintState {
                window.set_focus_handle(&self.focus_handle, cx);
            }
            fn paint(
                &mut self,
                _: Option<&GlobalElementId>,
                _: Option<&InspectorElementId>,
                _: Bounds<Pixels>,
                _: &mut Self::RequestLayoutState,
                _: &mut Self::PrepaintState,
                window: &mut Window,
                cx: &mut App,
            ) {
                let mut key_context = KeyContext::default();
                key_context.add("Terminal");
                window.set_key_context(key_context);
                window.handle_input(&self.focus_handle, self.clone(), cx);
                window.on_action(std::any::TypeId::of::<TestAction>(), |_, _, _, _| {});
            }
        }
        impl IntoElement for CustomElement {
            type Element = Self;

            fn into_element(self) -> Self::Element {
                self
            }
        }

        impl InputHandler for CustomElement {
            fn selected_text_range(
                &mut self,
                _: bool,
                _: &mut Window,
                _: &mut App,
            ) -> Option<UTF16Selection> {
                None
            }

            fn marked_text_range(&mut self, _: &mut Window, _: &mut App) -> Option<Range<usize>> {
                None
            }

            fn text_for_range(
                &mut self,
                _: Range<usize>,
                _: &mut Option<Range<usize>>,
                _: &mut Window,
                _: &mut App,
            ) -> Option<String> {
                None
            }

            fn replace_text_in_range(
                &mut self,
                replacement_range: Option<Range<usize>>,
                text: &str,
                _: &mut Window,
                _: &mut App,
            ) {
                if replacement_range.is_some() {
                    unimplemented!()
                }
                self.text.borrow_mut().push_str(text)
            }

            fn replace_and_mark_text_in_range(
                &mut self,
                replacement_range: Option<Range<usize>>,
                new_text: &str,
                _: Option<Range<usize>>,
                _: &mut Window,
                _: &mut App,
            ) {
                if replacement_range.is_some() {
                    unimplemented!()
                }
                self.text.borrow_mut().push_str(new_text)
            }

            fn unmark_text(&mut self, _: &mut Window, _: &mut App) {}

            fn bounds_for_range(
                &mut self,
                _: Range<usize>,
                _: &mut Window,
                _: &mut App,
            ) -> Option<Bounds<Pixels>> {
                None
            }

            fn character_index_for_point(
                &mut self,
                _: Point<Pixels>,
                _: &mut Window,
                _: &mut App,
            ) -> Option<usize> {
                None
            }
        }
        impl Render for CustomElement {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                self.clone()
            }
        }

        cx.update(|cx| {
            cx.bind_keys([KeyBinding::new("ctrl-b", TestAction, Some("Terminal"))]);
            cx.bind_keys([KeyBinding::new("ctrl-b h", TestAction, Some("Terminal"))]);
            cx.bind_keys([KeyBinding::new("ctrl-d", TestAction, None)]);
            cx.bind_keys([KeyBinding::new("ctrl-d h", TestAction, None)]);
        });

        let (test, cx) = cx.add_window_view(|_, cx| CustomElement::new(cx));
        let focus_handle = test.update(cx, |test, _| test.focus_handle.clone());

        let pending_input_changed_count = Rc::new(RefCell::new(0usize));
        let pending_input_changed_count_for_observer = pending_input_changed_count.clone();

        struct PendingInputObserver {
            _subscription: Subscription,
        }

        let _observer = cx.update(|window, cx| {
            cx.new(|cx| PendingInputObserver {
                _subscription: cx.observe_pending_input(window, move |_, _, _| {
                    *pending_input_changed_count_for_observer.borrow_mut() += 1;
                }),
            })
        });

        cx.update(|window, _| {
            window.focus(&focus_handle);
            window.activate_window();
        });
        cx.simulate_keystrokes("ctrl-b");
        let count_before_blur = *pending_input_changed_count.borrow();

        cx.update(|window, cx| {
            assert!(window.has_pending_keystrokes());
            window.blur(cx);
            assert!(!window.has_pending_keystrokes());
            assert!(window.pending_input_is_none());
        });

        cx.update(|_, _| {
            assert!(*pending_input_changed_count.borrow() > count_before_blur);
        });

        cx.update(|window, cx| window.disable_focus(cx));
        cx.simulate_keystrokes("ctrl-d");

        cx.update(|window, cx| {
            assert!(window.has_pending_keystrokes());
            window.blur(cx);
            assert!(window.pending_input_is_none());
        });
    }

    #[crate::test]
    fn test_standalone_modifier_dispatch(cx: &mut TestAppContext) {
        let (cx, action_count, _) =
            setup_pending_input_test(cx, [KeyBinding::new("shift", TestAction, Some("Terminal"))]);
        let intercepted_keystrokes = Rc::new(RefCell::new(Vec::new()));
        let should_consume = Rc::new(Cell::new(true));
        let terminal_context = terminal_context();
        let (observed_keystrokes, _observer) = capture_observed_keystrokes(cx);
        let _interceptor = cx.update(|_, cx| {
            cx.intercept_keystrokes({
                let intercepted_keystrokes = intercepted_keystrokes.clone();
                let should_consume = should_consume.clone();
                let terminal_context = terminal_context.clone();
                move |event, _, cx| {
                    assert!(event.action.is_none());
                    assert_eq!(event.context_stack, vec![terminal_context.clone()]);
                    intercepted_keystrokes
                        .borrow_mut()
                        .push(event.keystroke.clone());
                    if should_consume.get() {
                        cx.stop_propagation();
                    }
                }
            })
        });
        let shift = Keystroke::parse("shift").expect("valid keystroke");

        // Pressing Shift is not a keystroke. Consuming its recognized release suppresses
        // the binding.
        cx.simulate_modifiers_change(Modifiers::shift());
        assert!(intercepted_keystrokes.borrow().is_empty());
        cx.simulate_modifiers_change(Modifiers::none());
        assert_eq!(
            intercepted_keystrokes.borrow().as_slice(),
            std::slice::from_ref(&shift)
        );
        assert_eq!(action_count.get(), 0);
        assert!(observed_keystrokes.borrow().is_empty());

        // Without consumption, the same recognized release reaches keymap dispatch.
        should_consume.set(false);
        intercepted_keystrokes.borrow_mut().clear();
        cx.simulate_modifiers_change(Modifiers::shift());
        assert!(intercepted_keystrokes.borrow().is_empty());
        cx.simulate_modifiers_change(Modifiers::none());
        assert_eq!(
            intercepted_keystrokes.borrow().as_slice(),
            std::slice::from_ref(&shift)
        );
        assert_eq!(action_count.get(), 1);
        assert_eq!(
            observed_keystrokes.borrow().as_slice(),
            &[ObservedKeystroke {
                keystroke: shift,
                action_name: Some(TestAction::name(&TestAction)),
                context_stack: vec![terminal_context],
            }]
        );
    }

    #[crate::test]
    fn test_unbound_standalone_modifier_observation(cx: &mut TestAppContext) {
        let (cx, _, _) = setup_pending_input_test(cx, []);
        let terminal_context = terminal_context();
        let (observed_keystrokes, _observer) = capture_observed_keystrokes(cx);

        for (modifiers, key) in [
            (Modifiers::shift(), "shift"),
            (Modifiers::control(), "ctrl"),
            (Modifiers::alt(), "alt"),
            (Modifiers::command(), "cmd"),
            (function_modifier(), "fn"),
        ] {
            observed_keystrokes.borrow_mut().clear();
            cx.simulate_modifiers_change(modifiers);
            assert!(
                observed_keystrokes.borrow().is_empty(),
                "modifier press must not notify observers"
            );
            cx.simulate_modifiers_change(Modifiers::none());
            assert_eq!(
                observed_keystrokes.borrow().as_slice(),
                &[ObservedKeystroke {
                    keystroke: Keystroke::parse(key).expect("valid modifier"),
                    action_name: None,
                    context_stack: vec![terminal_context.clone()],
                }]
            );
        }
    }

    #[crate::test]
    fn test_raw_modifier_handler_can_suppress_observation(cx: &mut TestAppContext) {
        struct ModifierListenerTestView {
            focus_handle: FocusHandle,
            consume: Rc<Cell<bool>>,
            releases: Rc<Cell<usize>>,
        }

        impl Render for ModifierListenerTestView {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                use crate::{InteractiveElement as _, Styled as _};
                let consume = self.consume.clone();
                let releases = self.releases.clone();
                crate::div()
                    .key_context("Terminal")
                    .track_focus(&self.focus_handle)
                    .size_full()
                    .on_action(|_: &TestAction, _, cx| cx.propagate())
                    .on_modifiers_changed(move |event, _, cx| {
                        if !event.modifiers.modified() {
                            releases.set(releases.get() + 1);
                            if consume.get() {
                                cx.stop_propagation();
                            }
                        }
                    })
            }
        }

        cx.update(|cx| {
            cx.bind_keys([KeyBinding::new("shift", TestAction, Some("Terminal"))]);
        });
        let consume = Rc::new(Cell::new(true));
        let releases = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view(|_, cx| ModifierListenerTestView {
            focus_handle: cx.focus_handle(),
            consume: consume.clone(),
            releases: releases.clone(),
        });
        cx.update(|window, cx| {
            let focus_handle = view.read(cx).focus_handle.clone();
            window.focus(&focus_handle);
            window.activate_window();
        });
        let (observed_keystrokes, _observer) = capture_observed_keystrokes(cx);

        simulate_modifier_tap(cx, Modifiers::shift());
        assert_eq!(releases.get(), 1);
        assert!(observed_keystrokes.borrow().is_empty());

        consume.set(false);
        simulate_modifier_tap(cx, Modifiers::shift());
        assert_eq!(releases.get(), 2);
        assert_eq!(
            observed_keystrokes.borrow().as_slice(),
            &[ObservedKeystroke {
                keystroke: Keystroke::parse("shift").expect("valid modifier"),
                action_name: None,
                context_stack: vec![terminal_context()],
            }]
        );
    }

    #[crate::test]
    fn test_pending_modifier_observed_when_binding_resolves(cx: &mut TestAppContext) {
        let (cx, action_count, secondary_action_count) = setup_pending_input_test(
            cx,
            [
                KeyBinding::new("shift", TestAction, Some("Terminal")),
                KeyBinding::new("shift f1", SecondaryTestAction, Some("Terminal")),
            ],
        );
        let (observed_keystrokes, _observer) = capture_observed_keystrokes(cx);

        simulate_modifier_tap(cx, Modifiers::shift());
        assert!(observed_keystrokes.borrow().is_empty());
        assert_eq!(action_count.get(), 0);

        cx.executor().advance_clock(PENDING_INPUT_TIMEOUT);
        cx.run_until_parked();
        assert_eq!(action_count.get(), 1);
        assert_eq!(
            observed_keystrokes.borrow().as_slice(),
            &[ObservedKeystroke {
                keystroke: Keystroke::parse("shift").expect("valid modifier"),
                action_name: Some(TestAction::name(&TestAction)),
                context_stack: Vec::new(),
            }]
        );

        observed_keystrokes.borrow_mut().clear();
        simulate_modifier_tap(cx, Modifiers::shift());
        assert!(observed_keystrokes.borrow().is_empty());
        cx.simulate_keystrokes("f1");
        assert_eq!(action_count.get(), 1);
        assert_eq!(secondary_action_count.get(), 1);
        assert_eq!(
            observed_keystrokes.borrow().as_slice(),
            &[ObservedKeystroke {
                keystroke: Keystroke::parse("f1").expect("valid key"),
                action_name: Some(SecondaryTestAction::name(&SecondaryTestAction)),
                context_stack: vec![terminal_context()],
            }]
        );
    }

    #[crate::test]
    fn test_combined_shortcut_has_no_standalone_modifier(cx: &mut TestAppContext) {
        let (cx, action_count, _) = setup_pending_input_test(
            cx,
            [
                KeyBinding::new("shift", TestAction, Some("Terminal")),
                KeyBinding::new("shift-f1", TestAction, Some("Terminal")),
            ],
        );
        let intercepted_keystrokes = Rc::new(RefCell::new(Vec::new()));
        let (observed_keystrokes, _observer) = capture_observed_keystrokes(cx);
        let _interceptor = cx.update(|_, cx| {
            cx.intercept_keystrokes({
                let intercepted_keystrokes = intercepted_keystrokes.clone();
                move |event, _, _| {
                    intercepted_keystrokes
                        .borrow_mut()
                        .push(event.keystroke.clone());
                }
            })
        });

        // Shift-F1 is the only keystroke. Releasing Shift must not emit a second one.
        cx.simulate_modifiers_change(Modifiers::shift());
        assert!(intercepted_keystrokes.borrow().is_empty());
        cx.simulate_keystrokes("shift-f1");
        cx.simulate_modifiers_change(Modifiers::none());

        let shift_f1 = Keystroke::parse("shift-f1").expect("valid keystroke");
        assert_eq!(
            intercepted_keystrokes.borrow().as_slice(),
            std::slice::from_ref(&shift_f1)
        );
        assert_eq!(
            observed_keystrokes.borrow().as_slice(),
            &[ObservedKeystroke {
                keystroke: shift_f1,
                action_name: Some(TestAction::name(&TestAction)),
                context_stack: vec![terminal_context()],
            }]
        );
        assert_eq!(action_count.get(), 1);
    }

    #[crate::test]
    fn test_keystroke_input_preference(cx: &mut TestAppContext) {
        let (test, cx) = setup_altgr_test(cx);
        let intercepted_preferences = Rc::new(RefCell::new(Vec::new()));
        let observed_preferences = Rc::new(RefCell::new(Vec::new()));
        let _interceptor = cx.update(|_, cx| {
            cx.intercept_keystrokes({
                let intercepted_preferences = intercepted_preferences.clone();
                move |event, _, _| {
                    intercepted_preferences
                        .borrow_mut()
                        .push(event.input_preference);
                }
            })
        });
        let _observer = cx.update(|_, cx| {
            cx.observe_keystrokes({
                let observed_preferences = observed_preferences.clone();
                move |event, _, _| {
                    observed_preferences
                        .borrow_mut()
                        .push(event.input_preference);
                }
            })
        });

        for (prefer_character_input, accepts_text_input, input_preference) in [
            (true, true, InputPreference::CharacterInput),
            (false, true, InputPreference::KeyBindings),
            (true, false, InputPreference::KeyBindings),
            (false, false, InputPreference::KeyBindings),
        ] {
            test.update(cx, |test, _| {
                test.accepts_text_input.set(accepts_text_input);
                test.action_count.set(0);
            });
            intercepted_preferences.borrow_mut().clear();
            observed_preferences.borrow_mut().clear();
            let result = dispatch_altgr_key_down(cx, prefer_character_input);
            assert_eq!(
                result.propagate,
                input_preference == InputPreference::CharacterInput
            );
            assert_eq!(
                intercepted_preferences.borrow().as_slice(),
                &[input_preference]
            );
            assert_eq!(
                observed_preferences.borrow().as_slice(),
                &[input_preference]
            );
            test.read_with(cx, |test, _| {
                assert_eq!(
                    test.action_count.get(),
                    usize::from(input_preference == InputPreference::KeyBindings)
                );
            });
        }
    }

    #[crate::test]
    fn test_input_preference_rechecked_after_interception(cx: &mut TestAppContext) {
        let (test, cx) = setup_altgr_test(cx);
        let accepts_text_input = test.read_with(cx, |test, _| test.accepts_text_input.clone());
        let _interceptor = cx.update(|_, cx| {
            cx.intercept_keystrokes({
                let accepts_text_input = accepts_text_input.clone();
                move |event, _, _| {
                    assert_eq!(
                        event.input_preference == InputPreference::CharacterInput,
                        accepts_text_input.get()
                    );
                    accepts_text_input.set(!accepts_text_input.get());
                }
            })
        });
        let observed_preferences = Rc::new(RefCell::new(Vec::new()));
        let _observer = cx.update(|_, cx| {
            cx.observe_keystrokes({
                let observed_preferences = observed_preferences.clone();
                move |event, _, _| {
                    observed_preferences
                        .borrow_mut()
                        .push(event.input_preference);
                }
            })
        });

        for (initially_accepts_text_input, observed_preference) in [
            (true, InputPreference::KeyBindings),
            (false, InputPreference::CharacterInput),
        ] {
            accepts_text_input.set(initially_accepts_text_input);
            test.update(cx, |test, _| test.action_count.set(0));
            observed_preferences.borrow_mut().clear();
            let result = dispatch_altgr_key_down(cx, true);
            assert_eq!(result.propagate, !initially_accepts_text_input);
            assert_eq!(
                observed_preferences.borrow().as_slice(),
                &[observed_preference]
            );
            test.read_with(cx, |test, _| {
                assert_eq!(
                    test.action_count.get(),
                    usize::from(initially_accepts_text_input)
                );
            });
        }
    }

    #[crate::test]
    fn test_keystroke_interceptors_can_consume_preferred_character_input(cx: &mut TestAppContext) {
        let (test, cx) = setup_altgr_test(cx);
        let intercepted = Rc::new(Cell::new(false));
        let _interceptor = cx.update(|_, cx| {
            cx.intercept_keystrokes({
                let intercepted = intercepted.clone();
                move |event, _, cx| {
                    assert_eq!(event.input_preference, InputPreference::CharacterInput);
                    intercepted.set(true);
                    cx.stop_propagation();
                }
            })
        });

        let result = dispatch_altgr_key_down(cx, true);
        assert!(intercepted.get());
        assert!(!result.propagate);
        test.read_with(cx, |test, _| assert_eq!(test.action_count.get(), 0));
    }

    #[crate::test]
    fn test_consumed_modifier_preserves_pending_input(cx: &mut TestAppContext) {
        let (test, cx) = cx.add_window_view(|_, cx| PendingTextInputTestView::new(cx));
        cx.update(|window, cx| {
            let focus_handle = test.read(cx).focus_handle.clone();
            window.focus(&focus_handle);
            window.activate_window();
        });
        let _subscription = cx.update(|_, cx| {
            cx.intercept_keystrokes(|event, _, cx| {
                if event.keystroke.key == "shift" {
                    cx.stop_propagation();
                }
            })
        });

        // Exercise both paths that processing Shift would take: completing `j shift`, and
        // replaying `j` when `j k` no longer matches.
        for binding in ["j shift", "j k"] {
            cx.update(|_, cx| {
                cx.clear_key_bindings();
                cx.bind_keys([KeyBinding::new(binding, TestAction, Some("Terminal"))]);
            });
            test.update(cx, |test, _| test.text.borrow_mut().clear());
            cx.simulate_keystrokes("j");
            cx.executor().advance_clock(PENDING_INPUT_TIMEOUT / 2);
            cx.run_until_parked();
            let pending_keystrokes = cx.update(|window, _| {
                window
                    .pending_input_keystrokes()
                    .expect("pending input")
                    .to_vec()
            });

            // Consuming Shift must not complete or replay the pending `j`.
            cx.simulate_modifiers_change(Modifiers::shift());
            cx.simulate_modifiers_change(Modifiers::none());
            cx.update(|window, _| {
                assert_eq!(
                    window.pending_input_keystrokes().expect("pending input"),
                    pending_keystrokes.as_slice()
                );
                assert!(window.has_pending_keystrokes());
            });
            test.update(cx, |test, _| {
                assert_eq!(test.action_count.get(), 0);
                assert_eq!(test.text.borrow().as_str(), "");
            });

            // The unchanged timeout should replay `j` after its remaining half.
            cx.executor().advance_clock(PENDING_INPUT_TIMEOUT / 2);
            cx.run_until_parked();
            cx.update(|window, _| assert!(!window.has_pending_keystrokes()));
            test.update(cx, |test, _| {
                assert_eq!(test.action_count.get(), 0);
                assert_eq!(test.text.borrow().as_str(), "j");
            });
        }
    }
}
