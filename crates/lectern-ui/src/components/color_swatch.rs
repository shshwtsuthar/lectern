use std::rc::Rc;

use gpui::{
    App, ClickEvent, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement, Styled, Window, prelude::FluentBuilder as _, rems,
};
use gpui_base::Button as BaseButton;

use crate::PrimerTheme;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A circular color choice with explicit selected and accessible-name state.
#[derive(IntoElement)]
pub struct ColorSwatch {
    id: ElementId,
    label: String,
    color: Hsla,
    selected: bool,
    on_click: Option<ClickHandler>,
}

impl ColorSwatch {
    /// Creates an unselected color swatch.
    pub fn new(id: impl Into<ElementId>, label: impl Into<String>, color: Hsla) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            color,
            selected: false,
            on_click: None,
        }
    }

    /// Marks the swatch as the current choice.
    #[must_use]
    pub const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
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

impl RenderOnce for ColorSwatch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = PrimerTheme::current(cx);
        let selected = self.selected;
        let border = if selected {
            theme.focus.color
        } else {
            theme.border.muted
        };
        let label = if selected {
            format!("{} accent, selected", self.label)
        } else {
            format!("{} accent", self.label)
        };

        BaseButton::new(self.id)
            .accessibility_label(label)
            .selected(selected)
            .size(rems(2.))
            .rounded_full()
            .border(if selected {
                theme.focus.width
            } else {
                theme.border.thin
            })
            .border_color(border)
            .bg(self.color)
            .text_color(theme.selection.check_foreground)
            .font_weight(theme.typography.button_weight)
            .when(selected, |swatch| swatch.child("✓"))
            .cursor_pointer()
            .hover(|style| style.opacity(0.88))
            .active(|style| style.opacity(0.76))
            .focus_visible(move |style| {
                style
                    .border(theme.focus.width)
                    .border_color(theme.focus.color)
            })
            .when_some(self.on_click, |swatch, handler| {
                swatch.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .render(window, cx)
    }
}
