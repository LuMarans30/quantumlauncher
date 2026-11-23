//! Shared presentation for the Java and Horizon download stores. Store-specific
//! actions and filters are messages supplied by their respective managers.
use iced::{
    Alignment, Length,
    widget::{self, column, row},
};

use super::{Column, Element, button_with_icon, format_downloads, tsubtitle};
use crate::{
    icons,
    state::Message,
    stylesheet::{color::Color, styles::LauncherTheme},
};

pub const CARD_HEIGHT: u16 = 55;
pub const ACTION_WIDTH: u16 = 40;

pub fn page<'a>(
    header: Element<'a>,
    sidebar: widget::Scrollable<'a, Message, LauncherTheme>,
    results: Column<'a>,
) -> Element<'a> {
    column![header, row![sidebar, results]].into()
}

pub fn top_bar<'a>(
    back: Option<Message>,
    query: &str,
    search: Option<fn(String) -> Message>,
    stores: Column<'a>,
) -> Element<'a> {
    widget::container(
        row![
            button_with_icon(icons::back_s(12), "Back", 13)
                .padding([5, 8])
                .on_press_maybe(back),
            widget::text_input("Search...", query)
                .size(14)
                .on_input_maybe(search),
            widget::text("Store:").size(14).style(tsubtitle),
            stores,
        ]
        .align_y(Alignment::Center)
        .spacing(10),
    )
    .style(|n: &LauncherTheme| n.style_container_sharp_box(0.0, Color::ExtraDark))
    .padding([5, 10])
    .into()
}

pub fn type_heading<'a>() -> Element<'a> {
    row![icons::download_s(14), widget::text("Type:").size(18)]
        .align_y(Alignment::Center)
        .spacing(5)
        .into()
}

/// Continue the list when less than one more viewport remains below it.
pub fn needs_continuation(viewport: widget::scrollable::Viewport) -> bool {
    viewport.absolute_offset().y > viewport.content_bounds().height - 2.0 * viewport.bounds().height
}

pub fn results_list<'a>(
    contents: Column<'a>,
    id: &'static str,
    on_scroll: impl Fn(widget::scrollable::Viewport) -> Message + 'a,
) -> widget::Scrollable<'a, Message, LauncherTheme> {
    widget::scrollable(contents.spacing(5))
        .id(widget::scrollable::Id::new(id))
        .style(LauncherTheme::style_scrollable_flat_dark)
        .height(Length::Fill)
        .width(Length::Fill)
        .spacing(0)
        .on_scroll(on_scroll)
}

pub fn sidebar(contents: Column<'_>) -> widget::Scrollable<'_, Message, LauncherTheme> {
    widget::scrollable(contents.spacing(5).padding(10))
        .width(180)
        .height(Length::Fill)
        .style(LauncherTheme::style_scrollable_flat_extra_dark)
}

pub fn result_row<'a>(
    action: Element<'a>,
    image: Element<'a>,
    title: &'a str,
    description: &'a str,
    open: Message,
) -> Element<'a> {
    row![
        action,
        widget::button(
            row![
                image,
                column![
                    widget::text(title)
                        .wrapping(widget::text::Wrapping::None)
                        .shaping(widget::text::Shaping::Advanced)
                        .height(19),
                    widget::text(description)
                        .wrapping(widget::text::Wrapping::None)
                        .shaping(widget::text::Shaping::Advanced)
                        .size(12)
                        .style(tsubtitle),
                ]
                .spacing(2)
            ]
            .padding(8)
            .spacing(16)
        )
        .height(CARD_HEIGHT)
        .width(Length::Fill)
        .padding(0)
        .on_press(open)
    ]
    .spacing(5)
    .into()
}

pub fn uninstall(press: Option<Message>) -> Element<'static> {
    super::tooltip(
        widget::button(
            column![icons::bin()]
                .width(Length::Fill)
                .align_x(Alignment::Center),
        )
        .padding(10)
        .width(ACTION_WIDTH)
        .height(CARD_HEIGHT)
        .style(|t: &LauncherTheme, s| {
            t.style_button(
                s,
                crate::stylesheet::widgets::StyleButton::SemiDarkBorder([true; 4]),
            )
        })
        .on_press_maybe(press),
        "Uninstall",
        widget::tooltip::Position::FollowCursor,
    )
    .into()
}

pub fn action<'a>(
    icon: widget::Text<'a, LauncherTheme>,
    downloads: u64,
    press: Option<Message>,
) -> Element<'a> {
    widget::button(
        widget::center(
            column![
                icon,
                widget::text(format_downloads(downloads))
                    .size(10)
                    .style(tsubtitle)
            ]
            .spacing(5)
            .align_x(Alignment::Center),
        )
        .style(|_| widget::container::Style::default()),
    )
    .width(ACTION_WIDTH)
    .height(CARD_HEIGHT)
    .padding(0)
    .on_press_maybe(press)
    .into()
}
