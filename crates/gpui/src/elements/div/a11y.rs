//! Explicit accessibility attributes, allocated only when an element sets them.

use crate::SharedString;

#[derive(Default)]
pub(crate) struct A11yAttributes {
    pub(super) override_role: Option<accesskit::Role>,
    pub(super) accessibility_id: Option<SharedString>,
    pub(super) aria_label: Option<SharedString>,
    pub(super) aria_description: Option<SharedString>,
    pub(super) aria_value: Option<SharedString>,
    pub(super) aria_placeholder: Option<SharedString>,
    pub(super) aria_disabled: bool,
    pub(super) aria_required: bool,
    pub(super) aria_invalid: Option<accesskit::Invalid>,
    pub(super) aria_modal: bool,
    pub(super) aria_keyshortcuts: Option<SharedString>,
    pub(super) aria_selected: Option<bool>,
    pub(super) aria_expanded: Option<bool>,
    pub(super) aria_toggled: Option<accesskit::Toggled>,
    pub(super) aria_numeric_value: Option<f64>,
    pub(super) aria_min_numeric_value: Option<f64>,
    pub(super) aria_max_numeric_value: Option<f64>,
    pub(super) aria_orientation: Option<accesskit::Orientation>,
    pub(super) aria_level: Option<usize>,
    pub(super) aria_position_in_set: Option<usize>,
    pub(super) aria_size_of_set: Option<usize>,
    pub(super) aria_row_index: Option<usize>,
    pub(super) aria_column_index: Option<usize>,
    pub(super) aria_row_count: Option<usize>,
    pub(super) aria_column_count: Option<usize>,
}

impl A11yAttributes {
    pub(super) fn role(&self) -> Option<accesskit::Role> {
        self.override_role
            .filter(|role| *role != accesskit::Role::GenericContainer)
    }

    pub(super) fn write_to(&self, node: &mut accesskit::Node) {
        if let Some(id) = &self.accessibility_id {
            node.set_author_id(id.to_string());
        }
        if let Some(label) = &self.aria_label {
            node.set_label(label.to_string());
        }
        if let Some(description) = &self.aria_description {
            node.set_description(description.to_string());
        }
        if let Some(value) = &self.aria_value {
            node.set_value(value.to_string());
        }
        if let Some(placeholder) = &self.aria_placeholder {
            node.set_placeholder(placeholder.to_string());
        }
        if self.aria_disabled {
            node.set_disabled();
        }
        if self.aria_required {
            node.set_required();
        }
        if let Some(invalid) = self.aria_invalid {
            node.set_invalid(invalid);
        }
        if self.aria_modal {
            node.set_modal();
        }
        if let Some(keyshortcuts) = &self.aria_keyshortcuts {
            node.set_keyboard_shortcut(keyshortcuts.to_string());
        }
        if let Some(selected) = self.aria_selected {
            node.set_selected(selected);
        }
        if let Some(expanded) = self.aria_expanded {
            node.set_expanded(expanded);
        }
        if let Some(toggled) = self.aria_toggled {
            node.set_toggled(toggled);
        }
        if let Some(value) = self.aria_numeric_value {
            node.set_numeric_value(value);
        }
        if let Some(value) = self.aria_min_numeric_value {
            node.set_min_numeric_value(value);
        }
        if let Some(value) = self.aria_max_numeric_value {
            node.set_max_numeric_value(value);
        }
        if let Some(orientation) = self.aria_orientation {
            node.set_orientation(orientation);
        }
        if let Some(level) = self.aria_level {
            node.set_level(level);
        }
        if let Some(position) = self.aria_position_in_set {
            node.set_position_in_set(position);
        }
        if let Some(size) = self.aria_size_of_set {
            node.set_size_of_set(size);
        }
        if let Some(index) = self.aria_row_index {
            node.set_row_index(index);
        }
        if let Some(index) = self.aria_column_index {
            node.set_column_index(index);
        }
        if let Some(count) = self.aria_row_count {
            node.set_row_count(count);
        }
        if let Some(count) = self.aria_column_count {
            node.set_column_count(count);
        }
    }
}
