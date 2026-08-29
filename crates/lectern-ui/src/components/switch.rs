use std::rc::Rc;

use gpui::{
    App, ClickEvent, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, Styled, Window, div, prelude::FluentBuilder as _, relative, rems,
};
use gpui_base::{Switch as BaseSwitch, SwitchThumb, SwitchTrack};

use crate::PrimerTheme;

type ChangeHandler = Rc<dyn Fn(bool, &ClickEvent, &mut Window, &mut App)>;

/// A compact, controlled Lectern switch with an adjacent visible label.
#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    label: SharedString,
    checked: bool,
    disabled: bool,
    on_change: Option<ChangeHandler>,
}

impl Switch {
    /// Creates a switch with stable identity and a required visible, accessible label.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            checked: false,
            disabled: false,
            on_change: None,
        }
    }

    /// Sets the application-controlled checked state.
    #[must_use]
    pub const fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Disables pointer and keyboard activation.
    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Handles pointer, Enter, and Space activation with the next checked state.
    #[must_use]
    pub fn on_change(
        mut self,
        handler: impl Fn(bool, &ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = PrimerTheme::current(cx);
        let track_width = rems(crate::generated::primitive_metadata::CONTROL_SMALL_SIZE);
        let track_height = theme.spacing.large;
        let track_inset = theme.border.thin * 2.;
        let thumb_size = track_height - track_inset * 2.;
        let checked = self.checked;
        let disabled = self.disabled;
        let label = self.label;
        let focus_color = theme.focus.color;
        let hover_background = if checked {
            theme.switch.checked_hover_background
        } else {
            theme.switch.unchecked_hover_background
        };

        BaseSwitch::new(self.id.clone())
            .checked(checked)
            .disabled(disabled)
            .accessibility_label(label.clone())
            .flex()
            .items_center()
            .gap(theme.spacing.small)
            .border(theme.focus.width)
            .border_color(gpui::transparent_black())
            .rounded(theme.button.radius)
            .when(!disabled, Styled::cursor_pointer)
            .focus_visible(move |style| style.border_color(focus_color))
            .child(
                SwitchTrack::new((self.id, "track"))
                    .checked(checked)
                    .disabled(disabled)
                    .w(track_width)
                    .h(track_height)
                    .flex()
                    .items_center()
                    .when(checked, Styled::justify_end)
                    .border(track_inset)
                    .border_color(gpui::transparent_black())
                    .rounded_full()
                    .bg(if disabled {
                        theme.switch.disabled_background
                    } else if checked {
                        theme.switch.checked_background
                    } else {
                        theme.switch.unchecked_background
                    })
                    .when(!disabled, |track| {
                        track.hover(move |style| style.bg(hover_background))
                    })
                    .child(
                        SwitchThumb::new(checked)
                            .disabled(disabled)
                            .size(thumb_size)
                            .rounded_full()
                            .bg(if disabled {
                                theme.switch.disabled_thumb
                            } else {
                                theme.switch.thumb
                            }),
                    ),
            )
            .child(
                div()
                    .text_size(theme.typography.body_size)
                    .font_weight(theme.typography.button_weight)
                    .line_height(relative(theme.typography.body_line_height))
                    .text_color(if disabled {
                        theme.button.default.disabled_foreground
                    } else {
                        theme.surface.foreground
                    })
                    .child(label),
            )
            .when_some(self.on_change, |control, handler| {
                control.on_change(move |next, event, window, cx| {
                    handler(next, event, window, cx);
                })
            })
            .render(window, cx)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        rc::Rc,
        sync::{Arc, Mutex},
    };

    use gpui::{
        Context, Element as _, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, ParentElement,
        Render, Role, TestAppContext, VisualTestContext, accesskit, canvas, div, point, px,
    };

    use super::*;

    struct Harness {
        disabled: bool,
        changes: Rc<Cell<usize>>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let changes = Rc::clone(&self.changes);
            div().size(px(160.)).child(
                Switch::new("switch", "Tactile covers")
                    .disabled(self.disabled)
                    .on_change(move |_, _, _, _| changes.set(changes.get() + 1)),
            )
        }
    }

    fn harness(
        cx: &mut TestAppContext,
        disabled: bool,
    ) -> (&mut VisualTestContext, Rc<Cell<usize>>) {
        let changes = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let changes = Rc::clone(&changes);
            move |_, _| Harness { disabled, changes }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (cx, changes)
    }

    #[gpui::test]
    fn pointer_enter_and_space_request_the_next_state(cx: &mut TestAppContext) {
        let (cx, changes) = harness(cx, false);
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        for key in ["enter", "space"] {
            let keystroke = Keystroke::parse(key).unwrap();
            cx.simulate_event(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            cx.simulate_event(KeyUpEvent { keystroke });
        }
        assert_eq!(changes.get(), 3);
    }

    #[gpui::test]
    fn disabled_switch_is_inert(cx: &mut TestAppContext) {
        let (cx, changes) = harness(cx, true);
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        cx.update(Window::focus_next);
        cx.simulate_keystrokes("enter space");
        assert_eq!(changes.get(), 0);
    }

    #[gpui::test]
    fn exposes_switch_role_label_and_toggled_state(cx: &mut TestAppContext) {
        type Captured = Arc<Mutex<Option<accesskit::Node>>>;
        struct Probe(Captured);
        impl Render for Probe {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let captured = Arc::clone(&self.0);
                canvas(
                    move |_, window, cx| {
                        let mut node = accesskit::Node::new(Role::Switch);
                        Switch::new("a11y", "Tactile covers")
                            .checked(true)
                            .on_change(|_, _, _, _| {})
                            .render(window, cx)
                            .into_element()
                            .write_a11y_info(&mut node);
                        *captured.lock().unwrap() = Some(node);
                    },
                    |_, (), _, _| {},
                )
            }
        }

        let captured: Captured = Arc::new(Mutex::new(None));
        let result = Arc::clone(&captured);
        let (_, cx) = cx.add_window_view(move |_, _| Probe(captured));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let node = result.lock().unwrap().take().unwrap();
        assert_eq!(node.role(), Role::Switch);
        assert_eq!(node.label(), Some("Tactile covers"));
        assert_eq!(node.toggled(), Some(accesskit::Toggled::True));
        assert!(node.supports_action(accesskit::Action::Click));
    }
}
