//! Interactive gallery for Lectern's Primer-inspired GPUI components.

use gpui::{
    App, Bounds, Context, Entity, Rems, Render, StatefulInteractiveElement, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, relative, rems, size,
};
use gpui_base::input::{InputState, TextareaState};
use gpui_platform::application;
use lectern_ui::{
    Button, ButtonSize, ButtonVariant, ColorMode, LecternAssets, PrimerTheme, TablerIcon, TextArea,
    TextInput, install_theme,
};

struct ComponentGallery {
    input: Entity<InputState>,
    textarea: Entity<TextareaState>,
}

const ROOT_REM_PX: f32 = 16.0;
const WINDOW_WIDTH_PX: f32 = 760.0;
const WINDOW_HEIGHT_PX: f32 = 760.0;

impl ComponentGallery {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search or create a tag")
                .default_value("Ursula K. Le Guin")
        });
        let textarea = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(5)
                .placeholder("Add a description")
                .default_value("A gallery sample with native selection, clipboard, undo, and IME.")
        });
        Self { input, textarea }
    }
}

impl Render for ComponentGallery {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = PrimerTheme::current(cx);
        let next_theme = match theme.mode() {
            ColorMode::Light => PrimerTheme::dark(),
            ColorMode::Dark => PrimerTheme::light(),
        };

        div()
            .id("component-gallery")
            .size_full()
            .overflow_y_scroll()
            .bg(theme.surface.background)
            .text_color(theme.surface.foreground)
            .p(theme.spacing.extra_large)
            .flex()
            .flex_col()
            .gap(theme.spacing.large)
            .child(
                div()
                    .text_size(theme.typography.title_size)
                    .font_weight(theme.typography.title_weight)
                    .line_height(relative(theme.typography.title_line_height))
                    .child("Component gallery"),
            )
            .child(format!("Theme: {:?}", theme.mode()))
            .child(
                Button::new("switch-theme", "Switch theme").on_click(move |_, window, cx| {
                    install_theme(cx, next_theme.clone());
                    window.refresh();
                }),
            )
            .child(row(
                "Default",
                ButtonVariant::Default,
                false,
                false,
                theme.spacing.small,
                theme.spacing.medium,
            ))
            .child(
                div()
                    .mt(theme.spacing.large)
                    .text_size(theme.typography.title_size)
                    .font_weight(theme.typography.title_weight)
                    .child("Text fields"),
            )
            .child(
                div()
                    .w(px(420.))
                    .flex()
                    .flex_col()
                    .gap(theme.spacing.small)
                    .child("Contributor")
                    .child(TextInput::new("gallery-input", "Contributor", &self.input)),
            )
            .child(
                div()
                    .w(px(420.))
                    .flex()
                    .flex_col()
                    .gap(theme.spacing.small)
                    .child("Description")
                    .child(
                        TextArea::new("gallery-textarea", "Description", &self.textarea)
                            .height(rems(7.)),
                    ),
            )
            .child(row(
                "Primary",
                ButtonVariant::Primary,
                false,
                false,
                theme.spacing.small,
                theme.spacing.medium,
            ))
            .child(row(
                "Danger",
                ButtonVariant::Danger,
                false,
                false,
                theme.spacing.small,
                theme.spacing.medium,
            ))
            .child(row(
                "With icon",
                ButtonVariant::Primary,
                true,
                false,
                theme.spacing.small,
                theme.spacing.medium,
            ))
            .child(row(
                "Disabled",
                ButtonVariant::Danger,
                true,
                true,
                theme.spacing.small,
                theme.spacing.medium,
            ))
    }
}

fn row(
    title: &'static str,
    variant: ButtonVariant,
    leading_icon: bool,
    disabled: bool,
    label_gap: Rems,
    control_gap: Rems,
) -> impl IntoElement {
    div().flex().flex_col().gap(label_gap).child(title).child(
        div()
            .flex()
            .items_center()
            .gap(control_gap)
            .child(button(
                format!("{title}-small"),
                "Small",
                ButtonSize::Small,
                variant,
                leading_icon,
                disabled,
            ))
            .child(button(
                format!("{title}-medium"),
                "Medium",
                ButtonSize::Medium,
                variant,
                leading_icon,
                disabled,
            ))
            .child(button(
                format!("{title}-large"),
                "Large",
                ButtonSize::Large,
                variant,
                leading_icon,
                disabled,
            )),
    )
}

fn button(
    id: String,
    label: &'static str,
    size: ButtonSize,
    variant: ButtonVariant,
    leading_icon: bool,
    disabled: bool,
) -> Button {
    Button::new(id, label)
        .size(size)
        .variant(variant)
        .disabled(disabled)
        .when(leading_icon, |button| {
            button.leading_icon(TablerIcon::Upload)
        })
}

fn main() {
    application()
        .with_assets(LecternAssets)
        .run(|cx: &mut App| {
            gpui_base::init(cx);
            install_theme(cx, PrimerTheme::light());
            let bounds =
                Bounds::centered(None, size(px(WINDOW_WIDTH_PX), px(WINDOW_HEIGHT_PX)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    window.set_rem_size(px(ROOT_REM_PX));
                    cx.new(|cx| ComponentGallery::new(window, cx))
                },
            )
            .expect("open component gallery");
            cx.activate(true);
        });
}
