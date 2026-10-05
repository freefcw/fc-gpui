//! Accessibility support, provided by [AccessKit][accesskit].
//!
//! There are user-facing guide-level docs [here](crate::_accessibility).
//!
//! ## Architecture
//!
//! ```text
//! GPUI <-> AccessKit <-> platform adapter <-> system accessibility APIs
//! ```
//!
//! In order for GPUI apps to be usable for people using assistive technology,
//! we must do a few things:
//! - Inform the system when the UI changes meaningfully. This includes:
//!   - Reporting new/removed/changed UI elements
//!   - *Not* reporting irrelevant UI changes, e.g. an invisible `div()` being
//!     added.
//!   - Reporting the appearance and capabilities of each UI element. For example:
//!     - What does this piece of text say?
//!     - How far along is this progress bar?
//!     - Can this node be focused?
//!     - Can this node have a value directly assigned? (e.g. a slider)
//! - Allowing the system to interact with the UI by dispatching actions to
//!   nodes. Note that AccessKit has its own [`Action`] type, which is not the
//!   [`crate::Action`] trait.
//! - Activate and deactivate accessibility features when requested by the
//!   system.
//!
//! The state for both lives in the [`A11y`] struct in this module.

pub(crate) mod debug;

use crate::{App, Bounds, FocusId, Pixels, Window};
use accesskit::{Action, NodeId, TreeUpdate};
use collections::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// The fixed AccessKit node ID used for the root of every window's a11y tree.
pub(crate) const ROOT_NODE_ID: NodeId = NodeId(0);

/// A listener for an accessibility action on a specific node.
pub(crate) type A11yActionListener =
    Box<dyn FnMut(Option<&accesskit::ActionData>, &mut Window, &mut App) + 'static>;

/// Per-window accessibility state.
///
/// Manages the AccessKit tree that is built each frame and the mappings
/// needed to dispatch incoming action requests back to the right elements.
pub(crate) struct A11y {
    /// Whether accessibility has been forcibly disabled for this window.
    force_disabled: bool,
    /// Whether a11y features have been requested by the system.
    ///
    /// Updated by AccessKit using callbacks provided to the adapter. Can change
    /// halfway through a frame.
    active_flag: Arc<AtomicBool>,
    /// Whether a11y features are active for *this specific frame*.
    ///
    /// At the start of each frame, we load [`Self::active_flag`] (using
    /// [`Self::sync_active_flag`]) and use this to determine whether we
    /// should construct a [`TreeUpdate`] for this frame. It's important that
    /// this value is stable within a frame, because the builder API exposed by
    /// this type maintains a stack of nodes and each must be pushed and popped
    /// exactly once.
    ///
    /// At the end of the frame, we re-call [`Self::sync_active_flag`] to
    /// determine whether we should actually send the finished [`TreeUpdate`].
    active_this_frame: bool,
    pub(crate) nodes: A11yNodeBuilder,
    pub(crate) focus_ids: FxHashMap<NodeId, FocusId>,
    pub(crate) node_bounds: FxHashMap<NodeId, Bounds<Pixels>>,
    pub(crate) action_listeners: FxHashMap<NodeId, Vec<(Action, A11yActionListener)>>,
    debug_tree_json: Option<String>,
}

impl A11y {
    pub(crate) fn new(active_flag: Arc<AtomicBool>, force_disabled: bool) -> Self {
        Self {
            force_disabled,
            active_flag,
            active_this_frame: false,
            nodes: A11yNodeBuilder::new(),
            focus_ids: FxHashMap::default(),
            node_bounds: FxHashMap::default(),
            action_listeners: FxHashMap::default(),
            debug_tree_json: None,
        }
    }

    /// Ensures that [`Self::is_active`] returns up to date information.
    ///
    /// See the docs for [`Self::active_flag`] and [`Self::active_this_frame`]
    /// for more commentary.
    pub(crate) fn sync_active_flag(&mut self) {
        self.active_this_frame = self.is_enabled() && self.active_flag.load(Ordering::SeqCst);
    }

    pub(crate) fn is_enabled(&self) -> bool {
        !self.force_disabled
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active_this_frame
    }

    /// Clear per-frame state and push the root node to start a new frame.
    pub(crate) fn begin_frame(&mut self) {
        self.focus_ids.clear();
        self.node_bounds.clear();
        self.action_listeners.clear();
        self.nodes.begin_frame();
    }

    /// Finalize the tree and produce a [`TreeUpdate`] for the platform adapter.
    pub(crate) fn end_frame(&mut self) -> TreeUpdate {
        let update = self.nodes.finalize();
        self.debug_tree_json = Some(debug::tree_update_to_json(&update));
        update
    }

    pub(crate) fn debug_tree_json(&self) -> Option<String> {
        self.debug_tree_json.clone()
    }
}

pub(crate) struct A11yNodeBuilder {
    ids_stack: SmallVec<[NodeId; 16]>,
    nodes_stack: SmallVec<[accesskit::Node; 16]>,
    /// This is the exact type required by accesskit, so we can't just make it a
    /// `HashMap<NodeId, Node>` to remove the need for `seen_ids`
    all_nodes: Vec<(NodeId, accesskit::Node)>,
    seen_ids: FxHashSet<NodeId>,
    focus: NodeId,
    #[cfg(debug_assertions)]
    has_set_focus: bool,
}

impl A11yNodeBuilder {
    fn new() -> Self {
        Self {
            ids_stack: SmallVec::new(),
            nodes_stack: SmallVec::new(),
            all_nodes: Vec::new(),
            seen_ids: FxHashSet::default(),
            focus: ROOT_NODE_ID,
            #[cfg(debug_assertions)]
            has_set_focus: false,
        }
    }

    /// Push a new node onto the stack. It becomes a child of the current
    /// top-of-stack node.
    ///
    /// Returns `true` if the node was successfully pushed.
    pub(crate) fn push(&mut self, id: NodeId, node: accesskit::Node) -> bool {
        debug_assert!(!self.ids_stack.is_empty(), "push called before push_root");

        if !self.seen_ids.insert(id) {
            debug_assert!(
                false,
                "Duplicate a11y node id: {id:?}. In a release build, this node would be silently discarded from the a11y tree."
            );
            return false;
        }

        if let Some(parent) = self.nodes_stack.last_mut() {
            parent.push_child(id);
        }
        self.ids_stack.push(id);
        self.nodes_stack.push(node);
        true
    }

    /// Pop the current node off the stack and finalize it into the all_nodes
    /// list.
    pub(crate) fn pop(&mut self) {
        debug_assert!(self.ids_stack.len() > 1, "pop would remove the root node");

        if let (Some(id), Some(node)) = (self.ids_stack.pop(), self.nodes_stack.pop()) {
            self.all_nodes.push((id, node));
        }
    }

    /// Push the root node to start a new frame.
    fn begin_frame(&mut self) {
        self.all_nodes.clear();
        self.ids_stack.clear();
        self.nodes_stack.clear();
        self.seen_ids.clear();
        #[cfg(debug_assertions)]
        {
            self.has_set_focus = false;
        }
        let root_node = accesskit::Node::new(accesskit::Role::Window);

        self.ids_stack.push(ROOT_NODE_ID);
        self.nodes_stack.push(root_node);
        self.focus = ROOT_NODE_ID;
    }

    /// Returns whether a node with the given ID has been pushed in this frame.
    pub(crate) fn has_node(&self, id: NodeId) -> bool {
        id == ROOT_NODE_ID || self.seen_ids.contains(&id)
    }

    /// Set the focused node for this frame.
    pub(crate) fn set_focus(&mut self, id: NodeId) {
        #[cfg(debug_assertions)]
        {
            debug_assert!(
                !self.has_set_focus,
                "set_focus called more than once in a single frame"
            );
            self.has_set_focus = true;
        }
        self.focus = id;
    }

    fn finalize(&mut self) -> TreeUpdate {
        debug_assert_eq!(self.ids_stack.len(), 1);
        debug_assert_eq!(self.ids_stack[0], ROOT_NODE_ID);

        if self.ids_stack.len() != 1 {
            log::error!(
                "a11y: Stack imbalance at end of frame: expected 1 (root), got {}. \
                 Some elements may have pushed without popping.",
                self.ids_stack.len()
            );
        }

        while !self.ids_stack.is_empty() {
            if let (Some(id), Some(node)) = (self.ids_stack.pop(), self.nodes_stack.pop()) {
                self.all_nodes.push((id, node));
            }
        }

        let nodes = std::mem::take(&mut self.all_nodes);
        let update = TreeUpdate {
            nodes,
            tree: Some(accesskit::TreeInfo::new(ROOT_NODE_ID)),
            tree_id: accesskit::TreeId::ROOT,
            focus: self.focus,
        };

        Self::repair_tree_update(update)
    }

    /// AccessKit panics on invalid [`TreeUpdate`]s. This function defensively
    /// checks invariants that AccessKit panics on and tries to fix them.
    fn repair_tree_update(mut update: TreeUpdate) -> TreeUpdate {
        let node_ids: FxHashSet<NodeId> = update.nodes.iter().map(|(id, _)| *id).collect();

        if !node_ids.contains(&update.focus) {
            log::error!(
                "a11y: Focused node {:?} is not in the tree ({} nodes). \
                 Falling back to root. This is a bug in the a11y tree builder.",
                update.focus,
                update.nodes.len()
            );
            update.focus = ROOT_NODE_ID;
        }

        for (id, node) in &mut update.nodes {
            let has_invalid_child = node
                .children()
                .iter()
                .any(|child_id| !node_ids.contains(child_id));
            if has_invalid_child {
                let children = node.children();
                let invalid_count = children
                    .iter()
                    .filter(|child_id| !node_ids.contains(child_id))
                    .count();
                log::error!(
                    "a11y: Node {:?} references {} children not present in the tree. \
                     Stripping invalid child references.",
                    id,
                    invalid_count
                );
                let valid: Vec<NodeId> = children
                    .iter()
                    .copied()
                    .filter(|child_id| node_ids.contains(child_id))
                    .collect();
                node.set_children(valid);
            }
        }

        update
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalizes_tree_with_root_and_focus() {
        let mut builder = A11yNodeBuilder::new();
        let child_id = NodeId(1);

        builder.begin_frame();
        assert!(builder.push(child_id, accesskit::Node::new(accesskit::Role::Button)));
        builder.set_focus(child_id);
        builder.pop();

        let update = builder.finalize();

        assert_eq!(update.tree_id, accesskit::TreeId::ROOT);
        assert_eq!(update.focus, child_id);
        assert!(update.tree.is_some());
        assert!(update.nodes.iter().any(|(id, _)| *id == ROOT_NODE_ID));
        assert!(update.nodes.iter().any(|(id, _)| *id == child_id));
    }

    #[test]
    fn active_flag_is_snapshotted_until_sync() {
        let active_flag = Arc::new(AtomicBool::new(false));
        let mut a11y = A11y::new(active_flag.clone(), false);

        a11y.sync_active_flag();
        assert!(!a11y.is_active());

        active_flag.store(true, Ordering::SeqCst);
        assert!(!a11y.is_active());

        a11y.sync_active_flag();
        assert!(a11y.is_active());
    }

    #[test]
    fn force_disabled_ignores_active_flag() {
        let active_flag = Arc::new(AtomicBool::new(true));
        let mut a11y = A11y::new(active_flag, true);

        a11y.sync_active_flag();

        assert!(!a11y.is_active());
    }

    #[test]
    fn accessibility_enabled_is_independent_of_activation() {
        for force_disabled in [false, true] {
            for active in [false, true] {
                let mut a11y = A11y::new(Arc::new(AtomicBool::new(active)), force_disabled);
                a11y.sync_active_flag();

                assert_eq!(a11y.is_enabled(), !force_disabled);
                assert_eq!(a11y.is_active(), !force_disabled && active);
            }
        }
    }

    #[test]
    fn invalid_focus_falls_back_to_root() {
        let mut builder = A11yNodeBuilder::new();

        builder.begin_frame();
        builder.set_focus(NodeId(999));

        let update = builder.finalize();

        assert_eq!(update.focus, ROOT_NODE_ID);
    }

    #[test]
    fn debug_tree_json_includes_form_control_state() {
        let active_flag = Arc::new(AtomicBool::new(true));
        let mut a11y = A11y::new(active_flag, false);
        let input_id = NodeId(7);
        let mut input = accesskit::Node::new(accesskit::Role::TextInput);
        input.set_value("alice@example.com");
        input.set_placeholder("name@example.com");
        input.set_disabled();
        input.set_required();
        input.set_invalid(accesskit::Invalid::True);
        input.set_modal();

        a11y.sync_active_flag();
        a11y.begin_frame();
        assert!(a11y.nodes.push(input_id, input));
        a11y.nodes.pop();
        let _ = a11y.end_frame();

        let json: serde_json::Value = serde_json::from_str(
            &a11y
                .debug_tree_json()
                .expect("form control state should be debuggable"),
        )
        .unwrap();
        let aria = &json["nodes"][1]["aria"];

        assert_eq!(aria["value"], "alice@example.com");
        assert_eq!(aria["placeholder"], "name@example.com");
        assert_eq!(aria["disabled"], true);
        assert_eq!(aria["required"], true);
        assert_eq!(aria["invalid"], "True");
        assert_eq!(aria["modal"], true);
    }

    #[test]
    fn debug_tree_json_contains_focus_hierarchy_and_aria_metadata() {
        let active_flag = Arc::new(AtomicBool::new(true));
        let mut a11y = A11y::new(active_flag, false);
        let button_id = NodeId(42);
        let mut button = accesskit::Node::new(accesskit::Role::Button);
        button.set_label("Save".to_string());
        button.set_description("Save the active document".to_string());
        button.set_keyboard_shortcut("Ctrl+S".to_string());

        a11y.sync_active_flag();
        a11y.begin_frame();
        assert!(a11y.nodes.push(button_id, button));
        a11y.nodes.set_focus(button_id);
        a11y.nodes.pop();
        let _ = a11y.end_frame();

        let json: serde_json::Value = serde_json::from_str(
            &a11y
                .debug_tree_json()
                .expect("a completed accessibility frame should be debuggable"),
        )
        .unwrap();

        assert_eq!(json["root"], "a");
        assert_eq!(json["focus"], "b");
        assert_eq!(json["nodes"][0]["id"], "a");
        assert_eq!(json["nodes"][0]["accesskit_id"], "0");
        assert_eq!(json["nodes"][0]["children"][0], "b");
        assert_eq!(json["nodes"][1]["accesskit_id"], "42");
        assert_eq!(json["nodes"][1]["aria"]["role"], "Button");
        assert_eq!(json["nodes"][1]["aria"]["label"], "Save");
        assert_eq!(
            json["nodes"][1]["aria"]["description"],
            "Save the active document"
        );
        assert_eq!(json["nodes"][1]["aria"]["keyboard_shortcut"], "Ctrl+S");
    }
}
