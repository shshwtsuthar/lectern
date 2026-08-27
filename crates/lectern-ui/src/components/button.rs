use std::rc::Rc;

use gpui::{
    App, ClickEvent, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window, prelude::FluentBuilder as _,
    relative, rems, svg,
};
use gpui_base::Button as BaseButton;

use crate::{PrimerTheme, TablerIcon};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Semantic presentation variants supported by Lectern's first Button port.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ButtonVariant {
    /// A neutral action.
    #[default]
    Default,
    /// The primary action in a view.
    Primary,
    /// A destructive action requiring deliberate confirmation.
    Danger,
}

/// Primer control sizes supported by Lectern's first Button port.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ButtonSize {
    /// Compact controls.
    Small,
    /// Standard controls.
    #[default]
    Medium,
    /// Prominent controls.
    Large,
}

/// A typed Lectern Button derived from Primer and backed by `gpui-base` interaction behavior.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    variant: ButtonVariant,
    size: ButtonSize,
    leading_icon: Option<TablerIcon>,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    /// Creates a Button with a stable identity and required accessible label.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            variant: ButtonVariant::default(),
            size: ButtonSize::default(),
            leading_icon: None,
            disabled: false,
            on_click: None,
        }
    }

    /// Sets the semantic Button variant.
    #[must_use]
    pub const fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Sets the control size.
    #[must_use]
    pub const fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// Adds a decorative leading Tabler icon.
    #[must_use]
    pub const fn leading_icon(mut self, icon: TablerIcon) -> Self {
        self.leading_icon = Some(icon);
        self
    }

    /// Disables pointer and keyboard activation.
    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Handles pointer, Enter, and Space activation.
    #[must_use]
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = PrimerTheme::current(cx);
        let colors = match self.variant {
            ButtonVariant::Default => theme.button.default,
            ButtonVariant::Primary => theme.button.primary,
            ButtonVariant::Danger => theme.button.danger,
        };
        let (height, padding, gap) = match self.size {
            ButtonSize::Small => (
                rems(crate::generated::primitive_metadata::CONTROL_SMALL_SIZE),
                rems(crate::generated::primitive_metadata::CONTROL_SMALL_PADDING_INLINE),
                rems(crate::generated::primitive_metadata::CONTROL_SMALL_GAP),
            ),
            ButtonSize::Medium => (
                rems(crate::generated::primitive_metadata::CONTROL_MEDIUM_SIZE),
                rems(crate::generated::primitive_metadata::CONTROL_MEDIUM_PADDING_INLINE),
                rems(crate::generated::primitive_metadata::CONTROL_MEDIUM_GAP),
            ),
            ButtonSize::Large => (
                rems(crate::generated::primitive_metadata::CONTROL_LARGE_SIZE),
                rems(crate::generated::primitive_metadata::CONTROL_LARGE_PADDING_INLINE),
                rems(crate::generated::primitive_metadata::CONTROL_LARGE_GAP),
            ),
        };
        let icon_color = if self.disabled {
            colors.disabled_icon
        } else {
            colors.icon
        };
        let disabled = self.disabled;
        let on_click = self.on_click;

        BaseButton::new(self.id)
            .accessibility_label(self.label.clone())
            .disabled(disabled)
            .h(height)
            .px(padding)
            .gap(gap)
            .border(theme.button.border_width)
            .border_color(colors.border)
            .rounded(theme.button.radius)
            .bg(colors.background)
            .text_color(colors.foreground)
            .text_size(theme.typography.body_size)
            .font_weight(theme.typography.button_weight)
            .line_height(relative(theme.typography.body_line_height))
            .when(!disabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| {
                        style
                            .bg(colors.hover_background)
                            .border_color(colors.hover_border)
                            .text_color(colors.hover_foreground)
                    })
                    .active(move |style| {
                        style
                            .bg(colors.active_background)
                            .border_color(colors.active_border)
                            .text_color(colors.active_foreground)
                    })
            })
            .focus_visible(move |style| {
                style
                    .border(theme.focus.width)
                    .border_color(theme.focus.color)
            })
            .styles(|styles| {
                styles.disabled(|style| {
                    style
                        .bg(colors.disabled_background)
                        .border_color(colors.disabled_border)
                        .text_color(colors.disabled_foreground)
                })
            })
            .when_some(self.leading_icon, move |button, icon| {
                button.child(
                    svg()
                        .path(icon.path())
                        .size(rems(crate::generated::primitive_metadata::ICON_SIZE_SMALL))
                        // GPUI SVGs do not inherit the parent button's text color.
                        .text_color(icon_color),
                )
            })
            .child(self.label)
            .when_some(on_click, |button, handler| {
                button.on_click(move |event, window, cx| handler(event, window, cx))
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
        clicks: Rc<Cell<usize>>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let clicks = Rc::clone(&self.clicks);
            div().size(px(120.)).child(
                Button::new("button", "Add books")
                    .variant(ButtonVariant::Primary)
                    .leading_icon(TablerIcon::Upload)
                    .disabled(self.disabled)
                    .on_click(move |_, _, _| clicks.set(clicks.get() + 1)),
            )
        }
    }

    fn harness(
        cx: &mut TestAppContext,
        disabled: bool,
    ) -> (&mut VisualTestContext, Rc<Cell<usize>>) {
        let clicks = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let clicks = Rc::clone(&clicks);
            move |_, _| Harness { disabled, clicks }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (cx, clicks)
    }

    #[gpui::test]
    fn pointer_enter_and_space_activate(cx: &mut TestAppContext) {
        let (cx, clicks) = harness(cx, false);
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
        assert_eq!(clicks.get(), 3);
    }

    #[gpui::test]
    fn disabled_button_is_inert(cx: &mut TestAppContext) {
        let (cx, clicks) = harness(cx, true);
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        cx.update(Window::focus_next);
        cx.simulate_keystrokes("enter space");
        assert_eq!(clicks.get(), 0);
    }

    #[gpui::test]
    fn exposes_button_role_label_and_click_action(cx: &mut TestAppContext) {
        type Captured = Arc<Mutex<Option<(accesskit::Node, accesskit::Node)>>>;
        struct Probe(Captured);
        impl Render for Probe {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let captured = Arc::clone(&self.0);
                canvas(
                    move |_, window, cx| {
                        let mut capture = |button: Button| {
                            let mut node = accesskit::Node::new(Role::Button);
                            button
                                .render(window, cx)
                                .into_element()
                                .write_a11y_info(&mut node);
                            node
                        };
                        let enabled =
                            capture(Button::new("a11y", "Add books").on_click(|_, _, _| {}));
                        let disabled = capture(
                            Button::new("a11y-disabled", "Add books")
                                .disabled(true)
                                .on_click(|_, _, _| {}),
                        );
                        *captured.lock().unwrap() = Some((enabled, disabled));
                    },
                    |_, (), _, _| {},
                )
            }
        }

        let captured: Captured = Arc::new(Mutex::new(None));
        let result = Arc::clone(&captured);
        let (_, cx) = cx.add_window_view(move |_, _| Probe(captured));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let (enabled, disabled) = result.lock().unwrap().take().unwrap();
        assert_eq!(enabled.role(), Role::Button);
        assert_eq!(enabled.label(), Some("Add books"));
        assert!(enabled.supports_action(accesskit::Action::Click));
        assert_eq!(disabled.role(), Role::Button);
        assert_eq!(disabled.label(), Some("Add books"));
        assert!(!disabled.supports_action(accesskit::Action::Click));
    }
}
