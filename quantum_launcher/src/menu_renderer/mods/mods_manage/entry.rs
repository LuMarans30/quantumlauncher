use iced::widget::{self, tooltip::Position};
use ql_mod_manager::store::{ModConfig, ModId, ModIndex, QueryType, SelectedMod};

use crate::{
    menu_renderer::{
        Element,
        content_row::{ContentBadge, ContentRow, ICON_SIZE, missing_icon},
        tooltip, tsubtitle,
    },
    state::{ImageState, ManageModsMessage, MenuEditMods, Message, ModListEntry},
    stylesheet::styles::LauncherTheme,
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
        ContentRow {
            indicator: mod_toggler_or_indicator(
                project_type,
                None,
                &self.file_data.mod_index,
                move |_| ManageModsMessage::ToggleOneLocal(local.clone()).into(),
                is_enabled,
                true,
            ),
            icon: None,
            name: file_name.strip_suffix(".disabled").unwrap_or(file_name),
            version: "",
            name_width: None,
            enabled: is_enabled,
            selected: self
                .selection
                .selected_mods
                .contains(&SelectedMod::Local(local.clone())),
            select: ManageModsMessage::SelectMod(local.0.clone(), None, project_type).into(),
        }
        .button(size.width)
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
        let toggle = mod_toggler_or_indicator(
            config.project_type,
            Some(config),
            &self.file_data.mod_index,
            move |_| ManageModsMessage::ToggleOne(id.clone()).into(),
            is_enabled,
            config.manually_installed,
        );
        let select = ContentRow {
            indicator: toggle,
            icon: Some(image),
            name: &config.name,
            version: &config.installed_version,
            enabled: is_enabled || !config.project_type.is_toggleable(),
            selected: is_selected,
            name_width: Some(self.ui_state.width_name),
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
    match project_type {
        QueryType::Mods => {
            if manually_installed {
                return widget::toggler(is_enabled).on_toggle(f).size(14).into();
            }
            tooltip(
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
            .into()
        }
        QueryType::Shaders => ContentBadge::Shader.view(),
        QueryType::ModPacks => ContentBadge::ModPack.view(),
        QueryType::DataPacks => ContentBadge::DataPack.view(),
        QueryType::ResourcePacks => ContentBadge::ResourcePack.view(),
    }
}
