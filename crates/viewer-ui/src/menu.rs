//! Context menu: a list of actions shown at a point, such as where the mouse was right-clicked.
//!
//! Choosing an item runs its action and closes the menu. Clicking outside the menu or pressing Esc closes it without
//! an action. The menu has no text input. The owner keeps the menu entity, draws it with [`overlay`] and drops it
//! when the menu emits [`MenuEvent::Dismissed`].
//!
//! An item can also act differently with modifier keys held, such as Shift+click. Its label follows the keys held
//! while the menu is open, so what a click would do is seen before clicking.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    Modifiers, ModifiersChangedEvent, Pixels, Point, SharedString, Window, anchored, deferred, div,
    prelude::*, px,
};

use crate::theme;

/// Action of a menu item, given the modifier keys held when it was clicked.
pub type MenuAction = Rc<dyn Fn(Modifiers, &mut Window, &mut App)>;

/// Label of a menu item for the modifier keys held.
type MenuLabel = Rc<dyn Fn(Modifiers) -> SharedString>;

enum Entry {
    Item {
        label: MenuLabel,
        action: Option<MenuAction>,
    },
    /// Muted text that cannot be chosen.
    Note(SharedString),
    Separator,
}

pub enum MenuEvent {
    /// The menu closed, after an item's action or without one.
    Dismissed,
}

pub struct Menu {
    focus_handle: FocusHandle,
    entries: Vec<Entry>,
}

impl EventEmitter<MenuEvent> for Menu {}

impl Focusable for Menu {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Menu {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            entries: Vec::new(),
        }
    }

    /// Adds an item that runs `action` when chosen.
    pub fn item(
        mut self,
        label: impl Into<SharedString>,
        action: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        let label = label.into();
        self.entries.push(Entry::Item {
            label: Rc::new(move |_| label.clone()),
            action: Some(Rc::new(move |_, window, cx| action(window, cx))),
        });
        self
    }

    /// Adds an item whose label and action follow the modifier keys held. The label is redrawn as keys are pressed
    /// and released while the menu is open, and the action gets the keys held when the item is clicked.
    pub fn item_with_modifiers(
        mut self,
        label: impl Fn(Modifiers) -> SharedString + 'static,
        action: impl Fn(Modifiers, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.entries.push(Entry::Item {
            label: Rc::new(label),
            action: Some(Rc::new(action)),
        });
        self
    }

    /// Adds an item shown grayed out, which cannot be chosen.
    pub fn disabled_item(mut self, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        self.entries.push(Entry::Item {
            label: Rc::new(move |_| label.clone()),
            action: None,
        });
        self
    }

    /// Adds a line of small muted text that cannot be chosen, such as what modifier keys do.
    pub fn note(mut self, text: impl Into<SharedString>) -> Self {
        self.entries.push(Entry::Note(text.into()));
        self
    }

    /// Adds a line between groups of items. Separators at either end or next to each other are not drawn.
    pub fn separator(mut self) -> Self {
        push_separator(&mut self.entries);
        self
    }

    pub fn is_empty(&self) -> bool {
        !self
            .entries
            .iter()
            .any(|entry| matches!(entry, Entry::Item { .. }))
    }

    /// Focuses the menu so that Esc closes it.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus_handle, cx);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.emit(MenuEvent::Dismissed);
        }
    }
}

impl Render for Menu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let modifiers = window.modifiers();
        let last = self.entries.len().saturating_sub(1);
        let entries = self.entries.iter().enumerate().filter_map(|(ix, entry)| {
            Some(match entry {
                Entry::Separator if ix == last => return None,
                Entry::Separator => div()
                    .my_1()
                    .h(px(1.))
                    .bg(theme::border())
                    .into_any_element(),
                Entry::Note(text) => div()
                    .px(theme::metrics().button_padding_x)
                    .py(px(2.))
                    .text_color(theme::text_muted())
                    .child(text.clone())
                    .into_any_element(),
                Entry::Item { label, action } => {
                    let item = div()
                        .id(("menu-item", ix))
                        .px(theme::metrics().button_padding_x)
                        .py(px(3.))
                        .rounded_sm()
                        .truncate()
                        .child(label(modifiers));
                    match action.clone() {
                        Some(action) => item
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::hover()))
                            .on_click(cx.listener(move |_, event: &ClickEvent, window, cx| {
                                action(event.modifiers(), window, cx);
                                cx.emit(MenuEvent::Dismissed);
                            }))
                            .into_any_element(),
                        None => item.text_color(theme::text_muted()).into_any_element(),
                    }
                }
            })
        });
        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_modifiers_changed(cx.listener(|_, _: &ModifiersChangedEvent, _, cx| cx.notify()))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(MenuEvent::Dismissed)))
            .occlude()
            .min_w(px(180.))
            .max_w(px(420.))
            .p_1()
            .flex()
            .flex_col()
            .rounded_md()
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .text_xs()
            .text_color(theme::text())
            .children(entries.collect::<Vec<AnyElement>>())
    }
}

fn push_separator(entries: &mut Vec<Entry>) {
    if !matches!(entries.last(), None | Some(Entry::Separator)) {
        entries.push(Entry::Separator);
    }
}

/// Draws `menu` above the window with its top left corner at `position`, kept inside the window.
pub fn overlay(menu: &gpui::Entity<Menu>, position: Point<Pixels>) -> impl IntoElement {
    deferred(
        anchored()
            .position(position)
            .snap_to_window_with_margin(px(8.))
            .child(menu.clone()),
    )
    .with_priority(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(entries: &[Entry]) -> Vec<&'static str> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Item {
                    action: Some(_), ..
                } => "item",
                Entry::Item { action: None, .. } => "disabled",
                Entry::Note(_) => "note",
                Entry::Separator => "separator",
            })
            .collect()
    }

    #[test]
    fn drops_leading_and_repeated_separators() {
        let item = |label: &str| {
            let label = SharedString::from(label.to_string());
            Entry::Item {
                label: Rc::new(move |_| label.clone()),
                action: Some(Rc::new(|_, _: &mut Window, _: &mut App| {})),
            }
        };
        let mut entries = Vec::new();
        push_separator(&mut entries);
        entries.push(item("a"));
        push_separator(&mut entries);
        push_separator(&mut entries);
        entries.push(Entry::Item {
            label: Rc::new(|_| "b".into()),
            action: None,
        });
        push_separator(&mut entries);
        entries.push(Entry::Note("c".into()));
        assert_eq!(
            kinds(&entries),
            ["item", "separator", "disabled", "separator", "note"]
        );
    }
}
