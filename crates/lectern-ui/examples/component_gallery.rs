//! Interactive gallery for Lectern's Primer-inspired GPUI components.

use gpui::{
    App, Bounds, Context, Rems, Render, StatefulInteractiveElement, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, relative, size,
};
use gpui_platform::application;
use lectern_ui::{
    Button, ButtonSize, ButtonVariant, ColorMode, LecternAssets, PrimerTheme, TablerIcon,
    install_theme,
};

struct ComponentGallery;

const ROOT_REM_PX: f32 = 16.0;
const WINDOW_WIDTH_PX: f32 = 760.0;
const WINDOW_HEIGHT_PX: f32 = 620.0;

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
                    .child("Button gallery"),
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
                    cx.new(|_| ComponentGallery)
                },
            )
            .expect("open component gallery");
            cx.activate(true);
        });
}
