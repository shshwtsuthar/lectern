use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, App, ClickEvent, ElementId, Hsla, InteractiveElement, IntoElement,
    ParentElement, Rems, RenderOnce, Role, SharedString, StatefulInteractiveElement, Styled,
    Window, div, prelude::FluentBuilder as _, relative,
};
use gpui_base::{Button as BaseButton, Popover};

use crate::PrimerTheme;

type OpenChangeHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;
type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A Lectern-styled anchored action menu backed by `gpui-base` popover behavior.
#[derive(IntoElement)]
pub struct ActionMenu {
    id: ElementId,
    trigger: AnyElement,
    content: AnyElement,
    width: Rems,
    open: Option<bool>,
    on_open_change: Option<OpenChangeHandler>,
}

impl ActionMenu {
    /// Creates an anchored menu from application-owned trigger and content elements.
    pub fn new(
        id: impl Into<ElementId>,
        trigger: impl IntoElement,
        content: impl IntoElement,
    ) -> Self {
        Self {
            id: id.into(),
            trigger: trigger.into_any_element(),
            content: content.into_any_element(),
            width: gpui::rems(15.),
            open: None,
            on_open_change: None,
        }
    }

    /// Sets the menu surface width.
    #[must_use]
    pub const fn width(mut self, width: Rems) -> Self {
        self.width = width;
        self
    }

    /// Controls the open state from application state.
    #[must_use]
    pub const fn open(mut self, open: bool) -> Self {
        self.open = Some(open);
        self
    }

    /// Reports pointer, Escape, and outside-click open-state changes.
    #[must_use]
    pub fn on_open_change(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ActionMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = PrimerTheme::current(cx);
        let trigger = self.trigger;
        let content = self.content;
        let width = self.width;

        Popover::new(self.id)
            .anchor(Anchor::TopLeft)
            .when_some(self.open, Popover::open)
            .trigger_with(move |_, _, _| trigger)
            .when_some(self.on_open_change, |popover, handler| {
                popover.on_open_change(move |open, window, cx| handler(open, window, cx))
            })
            .content(move |_, _, _| {
                div()
                    .id("action-menu-scroll")
                    .mt(theme.spacing.small)
                    .w(width)
                    .max_h(gpui::rems(20.))
                    .overflow_y_scroll()
                    .p(theme.spacing.small)
                    .border(theme.border.thin)
                    .border_color(theme.action_menu.border)
                    .rounded(theme.action_menu.radius)
                    .bg(theme.action_menu.background)
                    .child(content)
            })
            .render(window, cx)
    }
}

/// One compact selectable row inside an [`ActionMenu`].
#[derive(IntoElement)]
pub struct ActionListItem {
    id: ElementId,
    label: SharedString,
    selected: bool,
    disabled: bool,
    leading_color: Option<Hsla>,
    on_click: Option<ClickHandler>,
}

/// A compact selected-tag pill with a named color dot and optional removal action.
#[derive(IntoElement)]
pub struct TagChip {
    id: ElementId,
    name: SharedString,
    color: Hsla,
    disabled: bool,
    on_remove: Option<ClickHandler>,
}

/// A compact selected-entity pill with an optional removal action.
#[derive(IntoElement)]
pub struct EntityChip {
    id: ElementId,
    name: SharedString,
    removal_noun: SharedString,
    disabled: bool,
    on_remove: Option<ClickHandler>,
}

impl EntityChip {
    /// Creates a non-removable entity chip.
    pub fn new(id: impl Into<ElementId>, name: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            removal_noun: "series".into(),
            disabled: false,
            on_remove: None,
        }
    }

    /// Disables a removable chip.
    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Sets the entity noun used by the removal accessibility label.
    #[must_use]
    pub fn removal_noun(mut self, noun: impl Into<SharedString>) -> Self {
        self.removal_noun = noun.into();
        self
    }

    /// Makes the chip removable and supplies its activation handler.
    #[must_use]
    pub fn on_remove(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_remove = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for EntityChip {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = PrimerTheme::current(cx);
        let removable = self.on_remove.is_some();
        let disabled = self.disabled;
        let hover_background = theme.button.default.hover_background;
        let accessibility_label = if removable {
            format!("Remove {} {}", self.removal_noun, self.name)
        } else {
            self.name.to_string()
        };

        BaseButton::new(self.id)
            .accessibility_label(accessibility_label)
            .disabled(disabled || !removable)
            .h(gpui::rems(
                crate::generated::primitive_metadata::CONTROL_SMALL_SIZE,
            ))
            .px(theme.spacing.medium)
            .gap(theme.spacing.small)
            .rounded_full()
            .border(theme.button.border_width)
            .border_color(theme.button.default.border)
            .bg(theme.button.default.background)
            .text_color(theme.button.default.foreground)
            .text_size(theme.typography.body_size)
            .line_height(relative(theme.typography.body_line_height))
            .when(removable && !disabled, |chip| {
                chip.cursor_pointer()
                    .hover(move |style| style.bg(hover_background))
            })
            .child(self.name)
            .when(removable, |chip| chip.child("×"))
            .when_some(self.on_remove, |chip, handler| {
                chip.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .render(window, cx)
    }
}

impl TagChip {
    /// Creates a non-removable tag chip.
    pub fn new(id: impl Into<ElementId>, name: impl Into<SharedString>, color: Hsla) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            color,
            disabled: false,
            on_remove: None,
        }
    }

    /// Disables a removable chip.
    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Makes the chip removable and supplies its activation handler.
    #[must_use]
    pub fn on_remove(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_remove = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for TagChip {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = PrimerTheme::current(cx);
        let removable = self.on_remove.is_some();
        let disabled = self.disabled;
        let hover_background = theme.button.default.hover_background;
        let accessibility_label = if removable {
            format!("Remove tag {}", self.name)
        } else {
            self.name.to_string()
        };

        BaseButton::new(self.id)
            .accessibility_label(accessibility_label)
            .disabled(disabled || !removable)
            .h(gpui::rems(
                crate::generated::primitive_metadata::CONTROL_SMALL_SIZE,
            ))
            .px(theme.spacing.medium)
            .gap(theme.spacing.small)
            .rounded_full()
            .border(theme.button.border_width)
            .border_color(theme.button.default.border)
            .bg(theme.button.default.background)
            .text_color(theme.button.default.foreground)
            .text_size(theme.typography.body_size)
            .line_height(relative(theme.typography.body_line_height))
            .when(removable && !disabled, |chip| {
                chip.cursor_pointer()
                    .hover(move |style| style.bg(hover_background))
            })
            .child(
                div()
                    .size(theme.spacing.medium)
                    .rounded_full()
                    .bg(self.color),
            )
            .child(self.name)
            .when(removable, |chip| chip.child("×"))
            .when_some(self.on_remove, |chip, handler| {
                chip.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .render(window, cx)
    }
}

impl ActionListItem {
    /// Creates an unselected action-list option.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            selected: false,
            disabled: false,
            leading_color: None,
            on_click: None,
        }
    }

    /// Marks this option as selected.
    #[must_use]
    pub const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Disables this option.
    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Adds a non-semantic color dot before the label.
    #[must_use]
    pub const fn leading_color(mut self, color: Hsla) -> Self {
        self.leading_color = Some(color);
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

impl RenderOnce for ActionListItem {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = PrimerTheme::current(cx);
        let selected = self.selected;
        let disabled = self.disabled;
        let label = self.label;
        let hover_background = theme.action_menu.hover_background;
        let focus = theme.focus;
        let selected_background = theme.action_menu.selected_background;
        let disabled_foreground = theme.input.disabled_foreground;

        BaseButton::new(self.id)
            .role(Role::ListBoxOption)
            .accessibility_label(label.clone())
            .aria_selected(selected)
            .selected(selected)
            .disabled(disabled)
            .w_full()
            .h(theme.input.height)
            .px(theme.spacing.medium)
            .gap(theme.spacing.small)
            .justify_start()
            .rounded(theme.action_menu.item_radius)
            .bg(theme.action_menu.background)
            .text_color(theme.surface.foreground)
            .text_size(theme.typography.body_size)
            .line_height(relative(theme.typography.body_line_height))
            .when(!disabled, |item| {
                item.cursor_pointer()
                    .hover(move |style| style.bg(hover_background))
            })
            .focus_visible(move |style| style.border(focus.width).border_color(focus.color))
            .styles(|styles| {
                styles
                    .selected(|style| style.bg(selected_background))
                    .disabled(|style| style.text_color(disabled_foreground))
            })
            .when_some(self.leading_color, |item, color| {
                item.child(div().size(theme.spacing.medium).rounded_full().bg(color))
            })
            .when(selected, |item| item.child(div().flex_none().child("✓")))
            .child(div().min_w_0().flex_1().truncate().child(label))
            .when_some(self.on_click, |item, handler| {
                item.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .render(window, cx)
    }
}
