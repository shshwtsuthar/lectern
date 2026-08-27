use std::rc::Rc;

use gpui::{
    App, ClickEvent, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window, prelude::FluentBuilder as _,
    relative, rems, svg,
};
use gpui_base::Button as BaseButton;

use crate::{ButtonSize, PrimerTheme, TablerIcon};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A compact square icon control with a required accessible name.
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    label: SharedString,
    icon: TablerIcon,
    size: ButtonSize,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl IconButton {
    /// Creates an icon-only button. `label` is always exposed to assistive technology.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: TablerIcon) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon,
            size: ButtonSize::default(),
            disabled: false,
            on_click: None,
        }
    }

    /// Sets the control size.
    #[must_use]
    pub const fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
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

impl RenderOnce for IconButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = PrimerTheme::current(cx);
        let colors = theme.button.default;
        let side = match self.size {
            ButtonSize::Small => rems(crate::generated::primitive_metadata::CONTROL_SMALL_SIZE),
            ButtonSize::Medium => rems(crate::generated::primitive_metadata::CONTROL_MEDIUM_SIZE),
            ButtonSize::Large => rems(crate::generated::primitive_metadata::CONTROL_LARGE_SIZE),
        };
        let icon_color = if self.disabled {
            colors.disabled_icon
        } else {
            colors.icon
        };
        let disabled = self.disabled;

        BaseButton::new(self.id)
            .accessibility_label(self.label)
            .disabled(disabled)
            .size(side)
            .justify_center()
            .border(theme.button.border_width)
            .border_color(colors.border)
            .rounded(theme.button.radius)
            .bg(colors.background)
            .text_size(theme.typography.body_size)
            .line_height(relative(theme.typography.body_line_height))
            .when(!disabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| {
                        style
                            .bg(colors.hover_background)
                            .border_color(colors.hover_border)
                    })
                    .active(move |style| {
                        style
                            .bg(colors.active_background)
                            .border_color(colors.active_border)
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
                })
            })
            .child(
                svg()
                    .path(self.icon.path())
                    .size(rems(crate::generated::primitive_metadata::ICON_SIZE_SMALL))
                    .text_color(icon_color),
            )
            .when_some(self.on_click, |button, handler| {
                button.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .render(window, cx)
    }
}
