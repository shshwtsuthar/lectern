use gpui::{
    App, ElementId, Entity, InteractiveElement, IntoElement, MouseButton, ParentElement, Rems,
    RenderOnce, SharedString, Styled, Window, div, prelude::FluentBuilder as _, relative, rems,
};
use gpui_base::InputBase;
use gpui_base::input::{
    Input as BaseInput, InputEditorStyle, InputState, Textarea as BaseTextarea, TextareaState,
};

use crate::PrimerTheme;

/// A single-line Lectern text field backed by `gpui-base` editing behavior.
#[derive(IntoElement)]
pub struct TextInput {
    id: ElementId,
    accessibility_label: SharedString,
    state: Entity<InputState>,
}

impl TextInput {
    /// Creates a medium, full-width text field with a stable identity.
    pub fn new(
        id: impl Into<ElementId>,
        accessibility_label: impl Into<SharedString>,
        state: &Entity<InputState>,
    ) -> Self {
        Self {
            id: id.into(),
            accessibility_label: accessibility_label.into(),
            state: state.clone(),
        }
    }
}

impl RenderOnce for TextInput {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = PrimerTheme::current(cx);
        apply_editor_theme(&self.state, &theme, cx);
        let presentation = self.state.read(cx).presentation();
        let focused = presentation.focus_handle().is_focused(window);
        let disabled = presentation.is_disabled();
        let state = self.state.clone();

        InputBase::new(self.id)
            .accessibility_label(self.accessibility_label)
            .focused(focused)
            .disabled(disabled)
            .w_full()
            .h(theme.input.height)
            .px(theme.input.padding_inline)
            .flex()
            .items_center()
            .overflow_hidden()
            .border(theme.input.border_width)
            .border_color(theme.input.border)
            .rounded(theme.input.radius)
            .bg(theme.input.background)
            .text_color(theme.input.foreground)
            .text_size(theme.typography.body_size)
            .line_height(relative(theme.typography.body_line_height))
            .styles(|styles| {
                styles
                    .focused(|style| {
                        style
                            .border(theme.focus.width)
                            .border_color(theme.focus.color)
                    })
                    .disabled(|style| {
                        style
                            .bg(theme.input.disabled_background)
                            .border_color(theme.input.disabled_border)
                            .text_color(theme.input.disabled_foreground)
                    })
            })
            .when(!disabled, |input| {
                input
                    .on_mouse_down_out(|_, window, _| window.blur())
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        state.update(cx, |state, cx| state.focus(window, cx));
                    })
            })
            .child(BaseInput::new(&self.state))
            .render(window, cx)
    }
}

/// A multi-line Lectern text field backed by `gpui-base` editing behavior.
#[derive(IntoElement)]
pub struct TextArea {
    id: ElementId,
    accessibility_label: SharedString,
    state: Entity<TextareaState>,
    height: Rems,
}

impl TextArea {
    /// Creates a full-width text area with a stable identity.
    pub fn new(
        id: impl Into<ElementId>,
        accessibility_label: impl Into<SharedString>,
        state: &Entity<TextareaState>,
    ) -> Self {
        Self {
            id: id.into(),
            accessibility_label: accessibility_label.into(),
            state: state.clone(),
            height: rems(8.),
        }
    }

    /// Sets the retained text area's visible frame height.
    #[must_use]
    pub const fn height(mut self, height: Rems) -> Self {
        self.height = height;
        self
    }
}

impl RenderOnce for TextArea {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = PrimerTheme::current(cx);
        apply_editor_theme(&self.state, &theme, cx);
        let presentation = self.state.read(cx).presentation();
        let focused = presentation.focus_handle().is_focused(window);
        let disabled = presentation.is_disabled();
        let state = self.state.clone();

        InputBase::new(self.id)
            .accessibility_label(self.accessibility_label)
            .focused(focused)
            .disabled(disabled)
            .w_full()
            .h(self.height)
            .px(theme.input.padding_inline)
            .py(theme.spacing.medium)
            .overflow_hidden()
            .border(theme.input.border_width)
            .border_color(theme.input.border)
            .rounded(theme.input.radius)
            .bg(theme.input.background)
            .text_color(theme.input.foreground)
            .text_size(theme.typography.body_size)
            .line_height(relative(theme.typography.body_line_height))
            .styles(|styles| {
                styles
                    .focused(|style| {
                        style
                            .border(theme.focus.width)
                            .border_color(theme.focus.color)
                    })
                    .disabled(|style| {
                        style
                            .bg(theme.input.disabled_background)
                            .border_color(theme.input.disabled_border)
                            .text_color(theme.input.disabled_foreground)
                    })
            })
            .when(!disabled, |input| {
                input
                    .on_mouse_down_out(|_, window, _| window.blur())
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        state.update(cx, |state, cx| state.focus(window, cx));
                    })
            })
            .child(div().size_full().child(BaseTextarea::new(&self.state)))
            .render(window, cx)
    }
}

fn apply_editor_theme<M: gpui_base::input::InputModeKind>(
    state: &Entity<gpui_base::input::InputBaseState<M>>,
    theme: &PrimerTheme,
    cx: &mut App,
) {
    state.update(cx, |state, _| {
        state.set_editor_style(InputEditorStyle {
            foreground: theme.input.foreground,
            muted_foreground: theme.input.placeholder,
            background: theme.input.background,
            border: theme.input.border,
            selection: theme.selection.background,
            caret: theme.input.foreground,
            ..InputEditorStyle::default()
        });
    });
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use gpui::{
        AppContext as _, Context, Element as _, Modifiers, Render, Role, TestAppContext, accesskit,
        canvas, point, px,
    };

    use super::*;

    struct Harness {
        input: Entity<InputState>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(280.))
                .h(px(48.))
                .child(TextInput::new("title", "Book title", &self.input))
        }
    }

    #[gpui::test]
    fn pointer_focus_and_keyboard_edit_use_retained_state(cx: &mut TestAppContext) {
        cx.update(gpui_base::init);
        let mut created = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Title"));
            created = Some(input.clone());
            Harness { input }
        });
        let input = created.unwrap();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_click(point(px(12.), px(12.)), Modifiers::default());
        cx.simulate_keystrokes("lectern");
        cx.update(|window, cx| {
            assert!(
                input
                    .read(cx)
                    .presentation()
                    .focus_handle()
                    .is_focused(window)
            );
            assert_eq!(input.read(cx).value().as_str(), "lectern");
        });
    }

    #[gpui::test]
    fn exposes_text_input_role_and_accessible_name(cx: &mut TestAppContext) {
        type Captured = Arc<Mutex<Option<accesskit::Node>>>;
        struct Probe {
            input: Entity<InputState>,
            captured: Captured,
        }
        impl Render for Probe {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let captured = Arc::clone(&self.captured);
                let input = self.input.clone();
                canvas(
                    move |_, window, cx| {
                        let mut node = accesskit::Node::new(Role::TextInput);
                        TextInput::new("a11y-title", "Book title", &input)
                            .render(window, cx)
                            .into_element()
                            .write_a11y_info(&mut node);
                        *captured.lock().unwrap() = Some(node);
                    },
                    |_, (), _, _| {},
                )
            }
        }

        cx.update(gpui_base::init);
        let captured: Captured = Arc::new(Mutex::new(None));
        let result = Arc::clone(&captured);
        let (_, cx) = cx.add_window_view(move |window, cx| Probe {
            input: cx.new(|cx| InputState::new(window, cx)),
            captured,
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let node = result.lock().unwrap().take().unwrap();
        assert_eq!(node.role(), Role::TextInput);
        assert_eq!(node.label(), Some("Book title"));
    }
}
