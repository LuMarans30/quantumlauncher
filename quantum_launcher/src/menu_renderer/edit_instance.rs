use crate::{
    icons,
    menu_renderer::{
        Column, FONT_MONO, button_with_icon, checkered_list, settings::PREFIX_EXPLANATION,
        tsubtitle,
    },
    state::{
        CustomJarState, EditInstanceMessage, HorizonUpdateState, ListMessage, MenuEditInstance,
        Message, NONE_JAR_NAME,
    },
    stylesheet::{color::Color, styles::LauncherTheme, widgets::StyleButton},
};
use iced::{
    Alignment, Length,
    widget::{self, column, row},
};
use ql_core::{Instance, InstanceKind};
use ql_core::{
    JavaVersion,
    json::{
        GlobalSettings, HorizonConfig,
        instance_config::{MainClassMode, PreLaunchPrefixMode},
    },
};

use super::Element;

const LOG_DIALOG: &str = r"Once disabled, logs will be printed in launcher STDOUT.
Run the launcher executable from the terminal/command prompt to see it";

const RES_DIALOG: &str = r"(Leave empty for default)
Common resolutions: 854x480, 1366x768, 1920x1080, 2560x1440, 3840x2160";

const JARMOD_DIALOG: &str = r"For *replacing* the Minecraft JAR, not adding to it.
To patch your existing JAR file, use 'Mods->Jarmod Patches'";

impl MenuEditInstance {
    pub fn view<'a>(
        &'a self,
        selected_instance: &Instance,
        jar_choices: Option<&'a CustomJarState>,
    ) -> Element<'a> {
        let horizon = self.config.horizon.as_ref();

        // Horizon always captures logs, so it has no logging toggle.
        let debug_logging = column![
            widget::Space::with_height(5),
            widget::checkbox(
                "DEBUG: Enable log system (recommended)",
                self.config.enable_logger.unwrap_or(true)
            )
            .on_toggle(|t| EditInstanceMessage::LoggingToggle(t).into()),
            widget::text(LOG_DIALOG).size(12).style(tsubtitle),
        ]
        .width(Length::Fill)
        .spacing(5);

        let mut items = vec![self.item_rename(selected_instance)];
        items.push(match horizon {
            Some(horizon) => item_horizon_build(horizon, &self.horizon_update),
            None => self.item_mem_alloc(),
        });
        // Instance type specific settings
        items.push(match selected_instance.kind {
            InstanceKind::Client => {
                let mut settings = column![resolution_dialog(
                    self.config.global_settings.as_ref(),
                    |n| EditInstanceMessage::WindowWidthChanged(n).into(),
                    |n| EditInstanceMessage::WindowHeightChanged(n).into(),
                )]
                .spacing(20);
                if horizon.is_none() {
                    settings = settings.push(debug_logging);
                }
                settings
            }
            // TODO: Add option to edit server.properties in user-friendly way
            InstanceKind::Server => column![widget::button("Edit server.properties")],
        });
        items.push(self.item_args());
        if horizon.is_none() {
            items.push(self.item_java_override());
            items.push(self.item_custom_jar(jar_choices));
        }
        items.push(item_footer(selected_instance.kind, horizon.is_some()));

        widget::scrollable(checkered_list(items))
            .style(LauncherTheme::style_scrollable_flat_extra_dark)
            .spacing(1)
            .into()
    }

    fn item_rename(&self, selected_instance: &Instance) -> Column<'_> {
        let kind_label = if self.config.horizon.is_some() {
            "Horizon".to_owned()
        } else {
            self.config.mod_type.to_string()
        };

        column![
            row![
                widget::text(selected_instance.get_name().to_owned())
                    .size(20)
                    .font(FONT_MONO)
            ]
            .push_maybe(
                (!self.state_rename.is_editing).then_some(
                    widget::button(
                        icons::edit_s(12).style(|t: &LauncherTheme| t.style_text(Color::Mid))
                    )
                    .style(|t: &LauncherTheme, s| t.style_button(s, StyleButton::FlatDark))
                    .on_press(EditInstanceMessage::RenameToggle.into())
                )
            )
            .spacing(5),
            widget::text!(
                "{} {}",
                kind_label,
                if selected_instance.is_server() {
                    "Server"
                } else {
                    "Client"
                }
            )
            .style(|t: &LauncherTheme| t.style_text(Color::Mid))
            .size(14),
        ]
        .width(Length::Fill)
        .spacing(5)
        .push_maybe(
            self.state_rename.is_editing.then_some(
                column![
                    widget::Space::with_height(1),
                    widget::text_input("Rename Instance", &self.state_rename.name)
                        .on_input(|n| EditInstanceMessage::RenameEdit(n).into()),
                    row![
                        widget::button(widget::text("Rename").size(12))
                            .on_press(EditInstanceMessage::RenameApply.into()),
                        widget::button(widget::text("Cancel").size(12))
                            .on_press(EditInstanceMessage::RenameToggle.into())
                    ]
                    .spacing(5)
                ]
                .spacing(5),
            ),
        )
    }

    fn item_args(&self) -> Column<'_> {
        let current_mode = self.config.global_java_args_enable.unwrap_or(true);
        let prefix_mode = self.config.pre_launch_prefix_mode.unwrap_or_default();

        let is_java = self.config.horizon.is_none();

        let sp = || widget::Space::with_height(5);

        let game_args = get_args_list(self.config.game_args.as_deref(), |n| {
            Message::EditInstance(EditInstanceMessage::GameArgs(n))
        });
        let env_vars = self.item_env_vars();
        let split = args_split_by_space(self.arg_split_by_space);

        if is_java {
            column![
                column![
                    row![
                        widget::text("Java arguments:").width(Length::Fill),
                        widget::checkbox("Use global arguments", current_mode)
                            .on_toggle(|t| EditInstanceMessage::JavaArgsModeChanged(t).into())
                            .style(|t: &LauncherTheme, s| {
                                t.style_checkbox(s, Some(Color::SecondLight))
                            })
                            .size(12)
                            .text_size(12)
                    ]
                    .align_y(Alignment::Center),
                    get_args_list(self.config.java_args.as_deref(), |n| Message::EditInstance(
                        EditInstanceMessage::JavaArgs(n)
                    )),
                    sp(),
                ]
                .spacing(7),
                "Game arguments:",
                game_args,
                sp(),
                self.item_args_prefix(prefix_mode),
                sp(),
                env_vars,
                sp(),
                split,
            ]
            .spacing(7)
            .width(Length::Fill)
        } else {
            column!["Game arguments:", game_args, sp(), env_vars, sp(), split,]
                .spacing(7)
                .width(Length::Fill)
        }
    }

    fn item_args_prefix(&self, prefix_mode: PreLaunchPrefixMode) -> Column<'_> {
        let checkbox = widget::checkbox("Use global prefix", !prefix_mode.is_disabled())
            .on_toggle(|t| {
                EditInstanceMessage::PreLaunchPrefixModeChanged(if t {
                    PreLaunchPrefixMode::default()
                } else {
                    PreLaunchPrefixMode::Disable
                })
                .into()
            })
            .style(|t: &LauncherTheme, s| t.style_checkbox(s, Some(Color::SecondLight)))
            .size(12)
            .text_size(12);

        column![
            row![
                widget::text("Pre-launch prefix:").width(Length::Fill),
                checkbox
            ]
            .align_y(Alignment::Center),
            row![get_args_list(
                self.config
                    .global_settings
                    .as_ref()
                    .and_then(|n| n.pre_launch_prefix.as_deref()),
                |n| EditInstanceMessage::PreLaunchPrefix(n).into(),
            )]
            .push_maybe(
                (!prefix_mode.is_disabled()).then_some(
                    widget::column(
                        [
                            PreLaunchPrefixMode::CombineGlobalLocal,
                            PreLaunchPrefixMode::CombineLocalGlobal,
                        ]
                        .iter()
                        .map(|n| {
                            widget::radio(n.get_description(), *n, Some(prefix_mode), |n| {
                                Message::EditInstance(
                                    EditInstanceMessage::PreLaunchPrefixModeChanged(n),
                                )
                            })
                            .style(|t: &LauncherTheme, s| t.style_radio(s, Color::SecondLight))
                            .size(10)
                            .text_size(10)
                            .into()
                        }),
                    )
                    .spacing(1),
                ),
            )
            .spacing(10),
            widget::text(PREFIX_EXPLANATION).size(12).style(tsubtitle),
        ]
        .width(Length::Fill)
        .spacing(7)
    }

    fn item_env_vars(&self) -> Column<'_> {
        column![
            "Environment variables:",
            get_args_list(
                self.config
                    .global_settings
                    .as_ref()
                    .and_then(|n| n.env_vars.as_deref()),
                |n| Message::EditInstance(EditInstanceMessage::EnvVars(n)),
            ),
            widget::text(
                "Format: KEY=VALUE (e.g., JAVA_OPTS=-Xmx2G)\nThese will be set when launching the instance"
            )
            .size(12)
            .style(tsubtitle),
        ]
        .width(Length::Fill)
        .spacing(7)
    }

    fn item_mem_alloc(&self) -> Column<'_> {
        // total RAM of system
        let total_mem = self.state_ram.system.total_memory() as f32 / 1024_f32.powf(2.0);
        const MEM_256_MB_IN_TWOS_EXPONENT: f32 = 8_f32;
        let mem_max_in_twos_exponent: f32 = total_mem.ln().max(256_f32.ln()) / 2_f32.ln();
        let mem_warning_threshold = ((total_mem) * 0.7) as usize; // 70%

        column![
            "Allocated memory",
            widget::text(
                r"Normal Minecraft: 2-3 GB
Old versions: 512 MB - 1 GB
Heavy modpacks / High settings: 4-8 GB+"
            )
            .size(12)
            .style(tsubtitle),
            widget::Space::with_height(5),
            row![
                widget::text(&self.state_ram.slider_text),
                widget::slider(
                    MEM_256_MB_IN_TWOS_EXPONENT..=mem_max_in_twos_exponent,
                    self.state_ram.slider_value,
                    |n| EditInstanceMessage::MemoryChanged(n).into()
                )
                .step(0.1),
            ]
            .align_y(Alignment::Center)
            .spacing(10),
            row![
                widget::text("Or enter directly:").size(12).style(tsubtitle),
                widget::text_input("2048", &self.state_ram.memory_input)
                    .on_input(|n| EditInstanceMessage::MemoryInputChanged(n).into())
                    .width(64)
                    .size(12),
                widget::text("MB").size(12).style(tsubtitle),
            ]
            .align_y(Alignment::Center)
            .spacing(5)
        ]
        .push_maybe(
            (self.config.ram_in_mb > mem_warning_threshold).then_some(
                widget::text(
                    "Warning: Very high RAM allocated! (More than 70% of total)\nYour system may struggle.",
                )
                .size(14),
            ),
        )
        .spacing(5)
    }

    fn item_java_override(&self) -> Column<'_> {
        fn radio(
            l: &str,
            a: bool,
            b: Option<bool>,
            f: impl Fn() -> Message,
        ) -> widget::Radio<'_, Message, LauncherTheme> {
            widget::radio(l, a, b, |n| if n { f() } else { Message::Nothing })
                .text_size(14)
                .size(14)
        }

        let java_override = self.config.java_override.as_deref().unwrap_or_default();
        let java_override_ver = self.config.java_override_version;
        column![
            "Java",
            radio(
                "Auto-managed by launcher",
                true,
                Some(java_override.trim().is_empty() && java_override_ver.is_none()),
                || EditInstanceMessage::JavaOverride(String::new()).into()
            ),
            row![radio(
                "Specific Java version",
                true,
                Some(java_override_ver.is_some()),
                || EditInstanceMessage::JavaOverrideVersion(25).into()
            )]
            .push_maybe(java_override_ver.map(|n| {
                widget::row(JavaVersion::ALL.iter().map(|v| {
                    let v = *v as usize;
                    widget::radio(format!("{v}"), v, Some(n), |v| {
                        EditInstanceMessage::JavaOverrideVersion(v).into()
                    })
                    .text_size(13)
                    .size(11)
                    .spacing(4)
                    .into()
                }))
                .spacing(5)
                .wrap()
            }))
            .spacing(16)
            .align_y(Alignment::Center),
            radio(
                "Custom path",
                true,
                Some(!java_override.trim().is_empty()),
                || EditInstanceMessage::JavaOverride("path/to/java".to_owned()).into() // ugly hack
            ),
            row![
                widget::Space::with_width(16),
                widget::text_input("Leave blank if none", java_override)
                    .size(14)
                    .font(FONT_MONO)
                    .on_input(|t| EditInstanceMessage::JavaOverride(t).into())
            ]
            .push_maybe(
                (!java_override.trim().is_empty()).then_some(
                    button_with_icon(icons::close_s(9), "", 13)
                        .padding([8.0, 11.0])
                        .on_press(EditInstanceMessage::JavaOverride(String::new()).into()),
                )
            )
            .push(
                button_with_icon(icons::folder_s(14), "", 13)
                    .padding([5, 10])
                    .on_press(Message::EditInstance(
                        EditInstanceMessage::BrowseJavaOverride
                    ))
            )
            .spacing(5)
        ]
        .spacing(5)
    }

    fn item_custom_jar<'a>(&'a self, jar_choices: Option<&'a CustomJarState>) -> Column<'a> {
        let picker: Element = if let Some(choices) = jar_choices {
            widget::pick_list(
                choices.choices.as_slice(),
                Some(
                    self.config
                        .custom_jar
                        .as_ref()
                        .map_or_else(|| NONE_JAR_NAME.to_owned(), |n| n.name.clone()),
                ),
                |t| EditInstanceMessage::CustomJarPathChanged(t).into(),
            )
            .into()
        } else {
            "Loading...".into()
        };

        column![
            row![widget::text("Custom JAR file").width(Length::Fill), picker]
                .align_y(Alignment::Center),
            widget::text(JARMOD_DIALOG).size(12).style(tsubtitle),
            widget::Space::with_height(10),
            widget::text("Main Class:"),
            widget::radio("Default", None, Some(self.main_class_mode), |t| {
                EditInstanceMessage::SetMainClass(t, None).into()
            })
            .size(14)
            .text_size(13),
            widget::radio(
                "Safe Mode (might fix crashes?)",
                Some(MainClassMode::SafeFallback),
                Some(self.main_class_mode),
                |t| EditInstanceMessage::SetMainClass(t, None).into()
            )
            .size(14)
            .text_size(13),
            row![
                widget::radio(
                    "Custom",
                    Some(MainClassMode::Custom),
                    Some(self.main_class_mode),
                    |t| EditInstanceMessage::SetMainClass(t, Some(String::new())).into()
                )
                .size(14)
                .text_size(13)
            ]
            .push_maybe(
                (self.main_class_mode == Some(MainClassMode::Custom)).then_some(
                    widget::text_input(
                        "Enter main class...",
                        self.config
                            .main_class_override
                            .as_deref()
                            .unwrap_or_default()
                    )
                    .on_input(|t| Message::EditInstance(EditInstanceMessage::SetMainClass(
                        Some(MainClassMode::Custom),
                        Some(t)
                    )))
                    .font(FONT_MONO)
                    .size(13)
                )
            )
            .spacing(10),
        ]
        .spacing(5)
    }
}

fn item_horizon_build(horizon: &HorizonConfig, update: &HorizonUpdateState) -> Column<'static> {
    let info = |label: &str, value: &str| -> widget::Text<'static, LauncherTheme> {
        widget::text!("{label}: {value}").size(14).font(FONT_MONO)
    };

    column![
        "Horizon build",
        column![
            info("Release", &horizon.build_tag),
            info("Build ID", &horizon.build_id),
            info("Minecraft", &horizon.minecraft_version),
        ]
        .spacing(2),
        widget::text(
            "Horizon runs natively, without Java, so the memory and Java settings don't apply."
        )
        .size(12)
        .style(tsubtitle),
        item_horizon_update(update),
    ]
    .spacing(5)
}

fn item_horizon_update(update: &HorizonUpdateState) -> Column<'static> {
    let check_button = |label: &'static str| {
        button_with_icon(icons::refresh_s(14), label, 13)
            .padding([4, 8])
            .on_press(Message::EditInstance(
                EditInstanceMessage::HorizonCheckUpdate,
            ))
    };
    let note = |text: &'static str| widget::text(text).size(12).style(tsubtitle);

    match update {
        HorizonUpdateState::Idle => column![
            check_button("Check for updates"),
            note(
                "Nightly is republished often. Checking asks GitHub for the release's signed checksum list; the build itself is only downloaded when you update."
            ),
        ],
        HorizonUpdateState::Checking => column![
            note("Checking for a newer Nightly build..."),
        ],
        HorizonUpdateState::UpToDate => column![
            note("This instance is on the latest Nightly build."),
            check_button("Check again"),
        ],
        HorizonUpdateState::Failed(error) => column![
            widget::text!("Couldn't check for updates:\n{error}")
                .size(12)
                .style(tsubtitle),
            check_button("Try again"),
        ],
        HorizonUpdateState::Available(update) => column![
            widget::text(if update.is_repair() {
                format!("This build is missing or incomplete: {}", update.id())
            } else {
                format!("A newer Nightly build is available: {}", update.id())
            })
                .size(12)
                .style(tsubtitle),
            button_with_icon(
                icons::version_download_s(14),
                if update.is_repair() { "Repair Horizon" } else { "Update Horizon" },
                13,
            )
                .padding([4, 8])
                .on_press(Message::EditInstance(EditInstanceMessage::HorizonUpdate)),
            note(
                "Your worlds, options and packs stay where they are. The update downloads the new build; the next launch prepares its resources, which can take a few minutes."
            ),
        ],
    }
    .spacing(5)
}

fn item_footer(
    kind: InstanceKind,
    is_horizon: bool,
) -> widget::Column<'static, Message, LauncherTheme> {
    match kind {
        InstanceKind::Client => Column::new()
            .push_maybe((!is_horizon).then(|| {
                column![
                    row![
                        button_with_icon(icons::version_download_s(14), "Reinstall Libraries", 13)
                            .padding([4, 8])
                            .on_press(Message::EditInstance(
                                EditInstanceMessage::ReinstallLibraries
                            )),
                        button_with_icon(icons::version_download_s(14), "Update Assets", 13)
                            .padding([4, 8])
                            .on_press(EditInstanceMessage::UpdateAssets.into()),
                    ]
                    .spacing(5)
                    .wrap(),
                    widget::horizontal_rule(2),
                ]
                .spacing(10)
            }))
            .push(
                button_with_icon(icons::bin(), "Delete Instance", 16)
                    .on_press(Message::DeleteInstanceMenu),
            )
            .spacing(10),
        InstanceKind::Server => {
            column![
                button_with_icon(icons::bin(), "Delete Server", 16)
                    .on_press(Message::DeleteInstanceMenu)
            ]
        }
    }
}

pub fn resolution_dialog<'a>(
    global_settings: Option<&GlobalSettings>,
    width: impl Fn(String) -> Message + 'a,
    height: impl Fn(String) -> Message + 'a,
) -> Column<'a> {
    column![
        "Custom Game Window Size (px):",
        widget::text(RES_DIALOG).size(12).style(tsubtitle),
        row![
            widget::text("Width:").size(14),
            widget::text_input(
                "854",
                &global_settings
                    .and_then(|n| n.window_width)
                    .map_or(String::new(), |w| w.to_string())
            )
            .size(14)
            .on_input(width)
            .width(100),
            widget::text("Height:").size(14),
            widget::text_input(
                "480",
                &global_settings
                    .and_then(|n| n.window_height)
                    .map_or(String::new(), |h| h.to_string())
            )
            .size(14)
            .on_input(height)
            .width(100),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .wrap(),
    ]
    .spacing(5)
}

pub fn get_args_list(
    args: Option<&[String]>,
    msg: impl Fn(ListMessage) -> Message + Clone + 'static,
) -> Element<'_> {
    fn opt(icon: widget::Text<'_, LauncherTheme>) -> widget::Button<'_, Message, LauncherTheme> {
        widget::button(icon)
            .padding([6, 8])
            .style(move |t: &LauncherTheme, s| t.style_button(s, StyleButton::FlatDark))
    }

    const ITEM_SIZE: u16 = 10;

    let args = args.unwrap_or_default();

    widget::Column::new()
        .push_maybe(
            (!args.is_empty()).then_some(widget::column(args.iter().enumerate().map(
                |(i, arg)| {
                    row![
                        opt(icons::bin_s(ITEM_SIZE)).on_press(msg(ListMessage::Delete(i))),
                        opt(icons::arrow_up_s(ITEM_SIZE)).on_press(msg(ListMessage::ShiftUp(i))),
                        opt(icons::arrow_down_s(ITEM_SIZE))
                            .on_press(msg(ListMessage::ShiftDown(i))),
                        widget::text_input("Enter argument...", arg)
                            .size(ITEM_SIZE + 4)
                            .font(FONT_MONO)
                            .on_input({
                                let msg = msg.clone();
                                move |n| msg(ListMessage::Edit(n, i))
                            })
                    ]
                    .align_y(Alignment::Center)
                    .into()
                },
            ))),
        )
        .push(row![get_args_list_add_button(msg)].spacing(10))
        .spacing(5)
        .width(Length::Fill)
        .into()
}

pub fn args_split_by_space(split: bool) -> widget::Checkbox<'static, Message, LauncherTheme> {
    widget::checkbox("Split arguments by spaces", split)
        .style(|t: &LauncherTheme, s| t.style_checkbox(s, Some(Color::SecondLight)))
        .size(12)
        .text_size(12)
        .on_toggle(|t| EditInstanceMessage::ToggleSplitArg(t).into())
}

fn get_args_list_add_button(
    msg: impl Fn(ListMessage) -> Message + Clone + 'static,
) -> widget::Button<'static, Message, LauncherTheme> {
    widget::button(
        row![icons::new_s(13), widget::text("Add").size(13)]
            .align_y(Alignment::Center)
            .spacing(8)
            .padding([1, 2]),
    )
    .style(move |t: &LauncherTheme, s| t.style_button(s, StyleButton::Round))
    .on_press(msg(ListMessage::Add))
}
