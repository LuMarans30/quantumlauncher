use super::{Column, Element, FONT_MONO, select_box};
use crate::{
    state::Message,
    stylesheet::{color::Color, styles::LauncherTheme},
};
use iced::{
    Alignment, Length,
    widget::{self, row},
};

const PADDING: iced::Padding = iced::Padding {
    top: 4.,
    bottom: 6.,
    right: 15.,
    left: 20.,
};
const SPACING: u16 = 16;
pub(crate) const ICON_SIZE: f32 = 18.;

/// Non-toggleable content types in the installed list.
pub(crate) enum ContentBadge {
    Shader,
    ResourcePack,
    ModPack,
    DataPack,
}

impl ContentBadge {
    pub fn view(self) -> Element<'static> {
        let (label, hint, color) = match self {
            Self::Shader => ("  S", "Shader", iced::Color::from_rgb8(0xB8, 0x6E, 0x3C)),
            Self::ResourcePack => (
                "  R",
                "Resource Pack",
                iced::Color::from_rgb8(0x5E, 0x7D, 0x61),
            ),
            Self::ModPack => ("  M", "Modpack", iced::Color::from_rgb8(0x6E, 0x5A, 0x8A)),
            Self::DataPack => ("  D", "Datapack", iced::Color::from_rgb8(0xA4, 0x4E, 0x4E)),
        };
        super::tooltip(
            widget::text(label).size(14).color(color).width(36),
            hint,
            widget::tooltip::Position::FollowCursor,
        )
        .into()
    }
}

pub(crate) fn installed_page<'a>(sidebar: Column<'a>, body: Element<'a>) -> Element<'a> {
    row![
        widget::scrollable(sidebar.padding(10).spacing(10))
            .height(Length::Fill)
            .style(LauncherTheme::style_scrollable_flat_dark),
        widget::container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|t: &LauncherTheme| t.style_container_sharp_box(0.0, Color::ExtraDark)),
    ]
    .into()
}

pub(crate) fn installed_list<'a>(
    rows: Column<'a>,
) -> widget::Scrollable<'a, Message, LauncherTheme> {
    widget::scrollable(rows)
        .direction(widget::scrollable::Direction::Both {
            vertical: widget::scrollable::Scrollbar::new(),
            horizontal: widget::scrollable::Scrollbar::new(),
        })
        .style(LauncherTheme::style_scrollable_flat_extra_dark)
        .width(Length::Fill)
        .height(Length::Fill)
}

pub(crate) fn missing_icon() -> Element<'static> {
    crate::icons::file_s(16)
        .width(ICON_SIZE)
        .height(ICON_SIZE)
        .into()
}

pub(crate) struct ContentToolbar<'a> {
    pub leading: Element<'a>,
    pub confirm_delete: bool,
    pub delete: Option<Message>,
    pub all_selected: bool,
    pub select_all: Option<Message>,
}

impl<'a> ContentToolbar<'a> {
    pub fn header(
        self,
        warning: Option<Element<'a>>,
        filters: Element<'a>,
        count: usize,
        search: Option<&str>,
        on_search: Option<fn(Option<String>) -> Message>,
    ) -> Column<'a> {
        let toggle = on_search.map(|set| {
            if search.is_some() {
                set(None)
            } else {
                Message::Multiple(vec![set(Some(String::new())), Message::CoreFocusNext])
            }
        });
        Column::new()
            .push_maybe(warning)
            .push(self.view(search.is_some(), toggle))
            .push(filters)
            .push(selection_hint(count))
            .push_maybe(search.map(|query| {
                widget::text_input("Search...", query)
                    .size(14)
                    .on_input_maybe(on_search.map(|set| move |value| set(Some(value))))
            }))
            .padding(10)
            .spacing(5)
    }

    fn view(self, active: bool, toggle: Option<Message>) -> Element<'a> {
        use super::subbutton_with_icon;
        use crate::{icons, stylesheet::widgets::StyleButton};
        row![
            self.leading,
            subbutton_with_icon(icons::search_s(12), "")
                .style(move |t: &LauncherTheme, s| t.style_button(
                    s,
                    if active {
                        StyleButton::Round
                    } else {
                        StyleButton::RoundDark
                    }
                ))
                .on_press_maybe(toggle),
            subbutton_with_icon(
                icons::bin_s(12),
                if self.confirm_delete {
                    "Confirm Delete"
                } else {
                    "Delete"
                }
            )
            .on_press_maybe(self.delete),
            subbutton_with_icon(
                icons::deselectall_s(12),
                if self.all_selected {
                    "Unselect All"
                } else {
                    "Select All"
                }
            )
            .on_press_maybe(self.select_all),
        ]
        .spacing(5)
        .wrap()
        .into()
    }
}

fn selection_hint(count: usize) -> Element<'static> {
    widget::text(match count {
        0 => "Select some content to perform actions on them".to_owned(),
        1 => "1 item selected".to_owned(),
        n => format!("{n} items selected"),
    })
    .size(11)
    .style(|t: &LauncherTheme| t.style_text(Color::Mid))
    .into()
}

/// Installed-content table row. The manager supplies its own indicator, selection
/// identity and metadata, without depending on the other manager's backend.
pub(crate) struct ContentRow<'a> {
    pub indicator: Element<'a>,
    pub icon: Option<Element<'a>>,
    pub name: &'a str,
    pub version: &'a str,
    /// Filename rows use their natural width; project rows reserve a name column.
    pub name_width: Option<f32>,
    pub enabled: bool,
    pub selected: bool,
    pub select: Message,
}
impl<'a> ContentRow<'a> {
    pub fn button(self, available_width: f32) -> widget::Button<'a, Message, LauncherTheme> {
        let name = widget::text(self.name)
            .shaping(widget::text::Shaping::Advanced)
            .wrapping(widget::text::Wrapping::None);
        let name = if self.name_width.is_some() {
            name.size(14)
        } else {
            name.font(FONT_MONO).size(13)
        };
        let name: Element = if self.enabled {
            name.style(|t: &LauncherTheme| t.style_text(Color::Light))
                .into()
        } else {
            widget::stack![
                name.style(|t: &LauncherTheme| t.style_text(Color::Mid)),
                row![
                    widget::horizontal_rule(1)
                        .style(|t: &LauncherTheme| t.style_rule(Color::Mid, 1))
                ]
                .height(Length::Fill)
                .align_y(Alignment::Center)
            ]
            .into()
        };
        let name =
            widget::column![name].push_maybe(self.name_width.map(|w| widget::Space::new(w, 0)));
        let mut row = row![self.indicator];
        if let Some(icon) = self.icon {
            row = row.push(icon);
            if self.name_width.is_some() {
                row = row.push(widget::Space::with_width(1));
            }
        }
        let row = row
            .push(name)
            .push_maybe((!self.version.is_empty()).then(|| {
                widget::text(self.version)
                    .style(move |t: &LauncherTheme| {
                        t.style_text(if self.enabled {
                            Color::Mid
                        } else {
                            Color::SecondDark
                        })
                    })
                    .font(FONT_MONO)
                    .size(12)
            }))
            .align_y(Alignment::Center)
            .padding(PADDING)
            .spacing(SPACING);
        // Fill the viewport for selection, but let long rows scroll horizontally.
        select_box(
            widget::column![row, widget::Space::new(available_width, 0)],
            self.selected,
            self.select,
        )
        .padding(0)
    }
}
