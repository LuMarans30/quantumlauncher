use iced::{
    Alignment, Length,
    widget::{self, row, tooltip::Position},
};
use ql_mod_manager::store::{ModConfig, ModId, ModIndex, QueryType, SelectedMod};

use crate::{
    menu_renderer::{Element, FONT_MONO, select_box, tooltip, tsubtitle},
    state::{ImageState, ManageModsMessage, MenuEditMods, Message, ModListEntry},
    stylesheet::{color::Color, styles::LauncherTheme},
};

const MOD_ENABLED_COL: Color = Color::Light;
const MOD_DISABLED_COL: Color = Color::Mid;

use crate::menu_renderer::content_row::{
    ContentRow, ICON_SIZE, MONO_CHAR_WIDTH, PADDING, SPACING, missing_icon,
};

impl MenuEditMods {
    pub(super) fn render_mod_entry<'a>(
        &'a self,
        entry: &'a ModListEntry,
        size: iced::Size,
        images: &'a ImageState,
    ) -> Element<'a> {
        match entry {
            ModListEntry::Downloaded { id, config } => {
                self.render_downloaded_mod_entry(size, images, id, config)
            }
            ModListEntry::Local(local) => self.render_local_mod_entry(size, local),
        }
    }

    fn render_local_mod_entry<'a>(
        &'a self,
        size: iced::Size,
        local: &'a ql_mod_manager::store::LocalMod,
    ) -> Element<'a> {
        let file_name = &*local.0;
        let project_type = local.1;

        let is_enabled = !file_name.ends_with(".disabled");
        let is_selected = self
            .selection
            .selected_mods
            .contains(&SelectedMod::Local(local.clone()));

        let label = file_name.strip_suffix(".disabled").unwrap_or(file_name);
        let label_len = label.len();

        let name = widget::text(label)
            .font(FONT_MONO)
            .shaping(widget::text::Shaping::Advanced)
            .size(13);

        let name: Element = if is_enabled {
            name.style(|t: &LauncherTheme| t.style_text(MOD_ENABLED_COL))
                .into()
        } else {
            widget::stack!(
                name.style(|t: &LauncherTheme| t.style_text(MOD_DISABLED_COL)),
                row![
                    widget::horizontal_rule(1)
                        .style(|t: &LauncherTheme| t.style_rule(MOD_DISABLED_COL, 1))
                ]
                .height(Length::Fill)
                .align_y(Alignment::Center)
            )
            .into()
        };

        select_box(
            row![
                mod_toggler_or_indicator(
                    project_type,
                    None,
                    &self.file_data.mod_index,
                    move |_| ManageModsMessage::ToggleOneLocal(local.clone()).into(),
                    is_enabled,
                    true
                ),
                name
            ]
            .push_maybe({
                // Measure the length of the text
                // then from there measure the space it would occupy
                // (only possible because monospace font)

                // This is for finding the filler space
                //
                // ║ some_mod.jar              ║
                // ║ some_other_mod.jar        ║
                //
                //  ╙═╦═══════════════╜  ╙═╦═══╜
                //  Measured by:         What we want
                //  `label_len`          to find

                let measured: f32 = (label_len as f32) * MONO_CHAR_WIDTH;
                let occupied = measured + PADDING.left + PADDING.right + 100.0;
                let space = size.width - occupied;
                (space > 0.0).then_some(widget::Space::with_width(space))
            })
            .padding(PADDING)
            .spacing(SPACING),
            is_selected,
            ManageModsMessage::SelectMod(local.0.clone(), None, project_type).into(),
        )
        .padding(0)
        .into()
    }

    fn render_downloaded_mod_entry<'a>(
        &'a self,
        size: iced::Size,
        images: &ImageState,
        id: &'a ModId,
        config: &'a ModConfig,
    ) -> Element<'a> {
        let is_enabled = config.enabled;
        let is_selected = self
            .selection
            .selected_mods
            .contains(&SelectedMod::Downloaded {
                name: config.name.clone(),
                id: (*id).clone(),
            });

        let image = config.icon_url.as_ref().map_or_else(missing_icon, |url| {
            images.view(Some(url), Some(ICON_SIZE), Some(ICON_SIZE))
        });

        let toggle: Element = mod_toggler_or_indicator(
            config.project_type,
            Some(config),
            &self.file_data.mod_index,
            move |_| ManageModsMessage::ToggleOne(id.clone()).into(),
            is_enabled,
            config.manually_installed,
        );

        let is_enabled_ui = is_enabled || !config.project_type.is_toggleable();
        let select = ContentRow {
            indicator: toggle,
            icon: image,
            name: &config.name,
            version: &config.installed_version,
            enabled: is_enabled_ui,
            selected: is_selected,
            name_width: self.ui_state.width_name,
            select: ManageModsMessage::SelectMod(
                config.name.clone(),
                Some(id.clone()),
                config.project_type,
            )
            .into(),
        }
        .button(size.width);

        self.with_mod_right_click(id, config, select).into()
    }

    fn with_mod_right_click<'a>(
        &self,
        id: &ModId,
        config: &ModConfig,
        entry: widget::Button<'a, Message, LauncherTheme>,
    ) -> widget::MouseArea<'a, Message, LauncherTheme> {
        let right_click_msg = ManageModsMessage::RightClick(id.clone()).into();

        widget::mouse_area(entry).on_right_press(
            if self.selection.selected_mods.len() > 1 && self.is_selected(id) {
                right_click_msg
            } else {
                Message::Multiple(vec![
                    ManageModsMessage::SelectEnsure(
                        config.name.clone(),
                        Some(id.clone()),
                        config.project_type,
                    )
                    .into(),
                    right_click_msg,
                ])
            },
        )
    }
}

fn mod_toggler_or_indicator<'a>(
    project_type: QueryType,
    config: Option<&ModConfig>,
    index: &'a ModIndex,
    f: impl Fn(bool) -> Message + 'a,
    is_enabled: bool,
    manually_installed: bool,
) -> Element<'a> {
    let size = 14;

    let (label, tooltip_text, color) = match project_type {
        QueryType::Mods => {
            if manually_installed {
                return widget::toggler(is_enabled).on_toggle(f).size(14).into();
            }
            return tooltip(
                widget::text(" Dep")
                    .size(12)
                    .color(iced::Color::from_rgb8(0x4E, 0x6E, 0x8A))
                    .width(36),
                widget::column![
                    widget::text!(
                        "{}Dependency of:",
                        if is_enabled { "" } else { "(Disabled) " }
                    )
                    .size(12)
                    .style(tsubtitle)
                ]
                .extend(config.into_iter().flat_map(|n| {
                    n.dependents.iter().filter_map(|id| {
                        index
                            .mods
                            .get(id)
                            .map(|m| widget::text(&*m.name).size(14).into())
                    })
                }))
                .push(
                    widget::text(
                        "\nTo enable/disable (not recommended),\ndo Right Click -> Toggle",
                    )
                    .size(12)
                    .style(tsubtitle),
                ),
                Position::Left,
            )
            .into();
        }
        QueryType::Shaders => ("  S", "Shader", iced::Color::from_rgb8(0xB8, 0x6E, 0x3C)),
        QueryType::ModPacks => ("  M", "Modpack", iced::Color::from_rgb8(0x6E, 0x5A, 0x8A)),
        QueryType::DataPacks => ("  D", "Datapack", iced::Color::from_rgb8(0xA4, 0x4E, 0x4E)),
        QueryType::ResourcePacks => (
            "  R",
            "Resource Pack",
            iced::Color::from_rgb8(0x5E, 0x7D, 0x61),
        ),
    };
    tooltip(
        widget::text(label).size(size).color(color).width(36),
        tooltip_text,
        Position::FollowCursor,
    )
    .into()
}
