use std::rc::Rc;

use gpui::{
    App, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window,
    div, prelude::FluentBuilder as _, relative, rems,
};
use gpui_base::Button as BaseButton;

use crate::PrimerTheme;

type RatingHandler = Rc<dyn Fn(&u8, &mut Window, &mut App)>;

const STAR_COUNT: u8 = 5;
const STAR_WIDTH_REM: f32 = 1.25;
const HALF_STAR_WIDTH_REM: f32 = STAR_WIDTH_REM / 2.0;

/// A compact five-star rating control with exact half-star activation targets.
#[derive(IntoElement)]
pub struct StarRating {
    id: SharedString,
    half_stars: u8,
    disabled: bool,
    on_change: Option<RatingHandler>,
}

impl StarRating {
    /// Creates a zero-to-five-star control from a bounded half-star value (`0..=10`).
    pub fn new(id: impl Into<SharedString>, half_stars: u8) -> Self {
        debug_assert!(half_stars <= STAR_COUNT * 2);
        Self {
            id: id.into(),
            half_stars: half_stars.min(STAR_COUNT * 2),
            disabled: false,
            on_change: None,
        }
    }

    /// Disables every pointer and keyboard rating target.
    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Handles activation with the selected half-star value (`1..=10`).
    #[must_use]
    pub fn on_change(mut self, handler: impl Fn(&u8, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for StarRating {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = PrimerTheme::current(cx);
        let foreground = if self.disabled {
            theme.rating.disabled
        } else {
            theme.rating.filled
        };
        let empty = if self.disabled {
            theme.rating.disabled
        } else {
            theme.rating.empty
        };
        let focus_width = theme.focus.width;
        let focus_color = theme.focus.color;

        div()
            .id(self.id.clone())
            .flex()
            .items_center()
            .children((0..STAR_COUNT).map(|star| {
                let first_half = star * 2 + 1;
                let filled_halves = self.half_stars.saturating_sub(star * 2).min(2);
                let fill_width = match filled_halves {
                    0 => rems(0.),
                    1 => rems(HALF_STAR_WIDTH_REM),
                    _ => rems(STAR_WIDTH_REM),
                };
                let id = self.id.clone();
                let on_change = self.on_change.clone();
                let disabled = self.disabled;

                div()
                    .relative()
                    .flex_none()
                    .w(rems(STAR_WIDTH_REM))
                    .h(rems(STAR_WIDTH_REM))
                    .text_size(rems(STAR_WIDTH_REM))
                    .line_height(relative(1.))
                    .child(div().absolute().inset_0().text_color(empty).child("☆"))
                    .when(filled_halves > 0, |star| {
                        star.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .h_full()
                                .w(fill_width)
                                .overflow_hidden()
                                .text_color(foreground)
                                .child(div().w(rems(STAR_WIDTH_REM)).child("★")),
                        )
                    })
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .children((0..2_u8).map(move |half| {
                                let value = first_half + half;
                                let on_change = on_change.clone();
                                BaseButton::new(format!("{id}-{value}"))
                                    .accessibility_label(format!(
                                        "Rate this book {} out of 5 stars",
                                        rating_label(value)
                                    ))
                                    .disabled(disabled)
                                    .w(rems(HALF_STAR_WIDTH_REM))
                                    .h(rems(STAR_WIDTH_REM))
                                    .p_0()
                                    .bg(gpui::transparent_black())
                                    .when(!disabled, Styled::cursor_pointer)
                                    .focus_visible(move |style| {
                                        style.border(focus_width).border_color(focus_color)
                                    })
                                    .when_some(on_change, move |button, handler| {
                                        button.on_click(move |_, window, cx| {
                                            handler(&value, window, cx);
                                        })
                                    })
                            })),
                    )
            }))
    }
}

fn rating_label(half_stars: u8) -> String {
    if half_stars.is_multiple_of(2) {
        (half_stars / 2).to_string()
    } else {
        format!("{}.5", half_stars / 2)
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{Context, Modifiers, ParentElement, Render, Styled, TestAppContext, div, point, px};

    use super::{StarRating, rating_label};

    struct Harness {
        disabled: bool,
        selected: Rc<Cell<u8>>,
    }

    impl Render for Harness {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut Context<Self>,
        ) -> impl gpui::IntoElement {
            let selected = Rc::clone(&self.selected);
            div().size(px(120.)).child(
                StarRating::new("rating", 0)
                    .disabled(self.disabled)
                    .on_change(move |value, _, _| selected.set(*value)),
            )
        }
    }

    #[test]
    fn accessibility_labels_use_exact_half_stars() {
        assert_eq!(rating_label(1), "0.5");
        assert_eq!(rating_label(7), "3.5");
        assert_eq!(rating_label(10), "5");
    }

    #[gpui::test]
    fn each_visual_half_star_is_an_exact_pointer_target(cx: &mut TestAppContext) {
        let selected = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let selected = Rc::clone(&selected);
            move |_, _| Harness {
                disabled: false,
                selected,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        for (x, expected) in [(px(5.), 1), (px(35.), 4), (px(85.), 9), (px(95.), 10)] {
            cx.simulate_click(point(x, px(10.)), Modifiers::default());
            assert_eq!(selected.get(), expected);
        }
    }

    #[gpui::test]
    fn disabled_rating_is_inert(cx: &mut TestAppContext) {
        let selected = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let selected = Rc::clone(&selected);
            move |_, _| Harness {
                disabled: true,
                selected,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        cx.simulate_click(point(px(85.), px(10.)), Modifiers::default());
        assert_eq!(selected.get(), 0);
    }
}
