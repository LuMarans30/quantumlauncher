//! Horizon manager: shared visual conventions, separate installation semantics.
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use iced::{
    Task,
    widget::{self, column},
};
use ql_core::{Instance, InstanceConfigJson, IntoStringError};
use ql_horizon::{
    HorizonError,
    catalog::{self, Page, Project, ProjectKind, Registry},
    content::{self, Kind, Package},
};

use crate::{
    icons,
    menu_renderer::{
        Column, Element, back_to_launch_screen, button_with_icon, content_row, content_store,
        error_popup, tsubtitle,
    },
    state::{ImageState, Launcher, Message, State},
};

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);
const CONTENT_TYPES: [(Option<ProjectKind>, &str); 3] = [
    (None, "All"),
    (Some(ProjectKind::Mod), "Mods"),
    (Some(ProjectKind::Shader), "Shaders"),
];
const CATALOG_SCROLL: &str = "HorizonContent:catalog";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Installed,
    Horizon,
}
#[derive(Default)]
enum BrowseResult {
    #[default]
    Idle,
    Loading,
    Ready(Page),
    Failed(String),
}

struct OpenProject {
    project: Project,
    request: u64,
    body: Result<Option<frostmark::MarkState>, String>,
    gallery: Vec<ql_mod_manager::store::GalleryItem>,
}

#[derive(Debug, Clone)]
pub struct InstalledPackage {
    package: Package,
    icon: Option<widget::image::Handle>,
}

impl InstalledPackage {
    fn load(game: &std::path::Path) -> Result<Vec<Self>, HorizonError> {
        Ok(content::list(game)?
            .into_iter()
            .map(|package| {
                let icon = package.thumbnail(game).map(|img| {
                    widget::image::Handle::from_rgba(img.width(), img.height(), img.into_raw())
                });
                Self { package, icon }
            })
            .collect())
    }
}

#[derive(Debug, Clone)]
pub struct ContentError {
    title: &'static str,
    message: String,
}

impl ContentError {
    fn other(message: impl ToString) -> Self {
        Self {
            title: "Horizon content error",
            message: message.to_string(),
        }
    }
}

impl From<HorizonError> for ContentError {
    fn from(error: HorizonError) -> Self {
        if matches!(error, HorizonError::UnsupportedNativeMod) {
            Self {
                title: "Unsupported Horizon mod",
                message: error.to_string(),
            }
        } else {
            Self::other(error)
        }
    }
}

pub struct Menu {
    pub instance: Instance,
    pub busy: bool,
    request: u64,
    packages: Vec<InstalledPackage>,
    error: Option<ContentError>,
    selected: std::collections::HashSet<(Kind, String)>,
    confirm_remove: bool,
    tab: Tab,
    installed_search: Option<String>,
    installed_kind: Option<ProjectKind>,
    query: String,
    registry: Option<Registry>,
    registry_kind: Option<ProjectKind>,
    browse_result: BrowseResult,
    browse_request: u64,
    detail: Option<OpenProject>,
    scroll: widget::scrollable::AbsoluteOffset,
    browse_job: Option<iced::task::Handle>,
}

#[derive(Debug, Clone)]
pub enum ContentMessage {
    Refresh,
    Tab(Tab),
    Query(String),
    InstalledSearch(Option<String>),
    InstalledFilter(Option<ProjectKind>),
    RetrySearch,
    Filter(Option<ProjectKind>),
    RegistryLoaded(u64, Result<Registry, String>),
    OpenProject(String),
    CloseProject,
    UpdateDescription(u64, frostmark::UpdateMsg),
    Scrolled(widget::scrollable::Viewport),
    Download(String),
    Uninstall(ProjectKind, String),
    PickFiles,
    Picked(u64, Vec<PathBuf>),
    Toggle(Kind, String, bool),
    Select(Kind, String),
    SelectAll,
    ConfirmRemove,
    Remove,
    Finished(u64, Vec<InstalledPackage>, Option<ContentError>),
    DismissError,
}
impl From<ContentMessage> for Message {
    fn from(value: ContentMessage) -> Self {
        Self::HorizonContent(value)
    }
}

enum Action {
    Refresh,
    Install(Vec<PathBuf>),
    Toggle(Kind, String, bool),
    Remove(Vec<(Kind, String)>),
    Download(String),
}

impl Menu {
    pub fn new(instance: Instance) -> Self {
        Self {
            instance,
            busy: false,
            request: 0,
            packages: Vec::new(),
            error: None,
            selected: Default::default(),
            confirm_remove: false,
            tab: Tab::Installed,
            installed_search: None,
            installed_kind: None,
            query: String::new(),
            registry: None,
            registry_kind: None,
            browse_result: BrowseResult::Idle,
            browse_request: 0,
            detail: None,
            scroll: Default::default(),
            browse_job: None,
        }
    }
    pub fn dismiss_error_message(&self) -> Option<Message> {
        self.error
            .as_ref()
            .map(|_| ContentMessage::DismissError.into())
    }

    pub fn back_message(&self) -> Message {
        if let Some(dismiss) = self.dismiss_error_message() {
            dismiss
        } else if self.detail.is_some() {
            ContentMessage::CloseProject.into()
        } else if self.tab == Tab::Installed {
            back_to_launch_screen(None)
        } else {
            ContentMessage::Tab(Tab::Installed).into()
        }
    }

    fn begin(&mut self) -> u64 {
        self.busy = true;
        self.confirm_remove = false;
        self.request = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        self.request
    }

    pub fn view<'a>(
        &'a self,
        running: bool,
        images: &'a ImageState,
        tick_timer: usize,
    ) -> Element<'a> {
        widget::stack![self.view_page(running, images, tick_timer)]
            .push_maybe(self.error.as_ref().map(|error| {
                error_popup(
                    error.title,
                    &error.message,
                    ContentMessage::DismissError.into(),
                )
            }))
            .into()
    }

    fn view_page<'a>(
        &'a self,
        running: bool,
        images: &'a ImageState,
        tick_timer: usize,
    ) -> Element<'a> {
        if let Some(detail) = &self.detail {
            let p = &detail.project;
            let request = detail.request;
            return crate::menu_renderer::ProjectDescription {
                title: &p.name,
                id: &p.id,
                summary: &p.summary,
                icon: p.icon_url.as_deref(),
                gallery: &detail.gallery,
                urls: &[],
                page_url: format!("https://playhorizon.dev/mods/{}", p.id),
            }
            .view(
                detail.body.as_ref(),
                self.back_message(),
                images,
                tick_timer,
                move |n| ContentMessage::UpdateDescription(request, n).into(),
            );
        }
        let locked = self.busy || running;
        let mut body = column![].spacing(5);
        if self.busy {
            body = body.push(widget::text("Working…"));
        }
        if running {
            body = body.push(widget::text("Stop Horizon before changing packages."));
        }
        if self.tab == Tab::Horizon {
            return self.browse(body, locked, images);
        }
        content_row::installed_page(
            self.installed_sidebar(locked),
            self.installed(body, locked).into(),
        )
    }

    fn installed_sidebar(&self, locked: bool) -> Column<'_> {
        column![
            crate::menu_renderer::mod_file_actions(
                (!self.busy).then(|| self.back_message()),
                Message::CoreOpenPath(self.instance.get_dot_minecraft_path()),
                (!locked).then_some(ContentMessage::PickFiles.into()),
            ),
            crate::menu_renderer::download_content_button(
                (!self.busy).then_some(ContentMessage::Tab(Tab::Horizon).into()),
            ),
        ]
        .push_maybe(self.packages.is_empty().then(|| {
            button_with_icon(icons::refresh_s(14), "Refresh", 14)
                .on_press_maybe((!self.busy).then_some(ContentMessage::Refresh.into()))
        }))
        .spacing(5)
    }

    fn installed<'a>(&'a self, mut body: Column<'a>, locked: bool) -> Column<'a> {
        if self.packages.is_empty() {
            if !self.busy {
                body = body.push("Download some mods to get started").push(
                    widget::button(widget::text("Browse Horizon registry").size(14))
                        .on_press(ContentMessage::Tab(Tab::Horizon).into()),
                );
            }
            return body.padding(10);
        }
        use crate::menu_renderer::subbutton_with_icon;
        let visible = self.visible_packages().collect::<Vec<_>>();
        let count = self.selected.len();
        use crate::menu_renderer::content_row::ContentToolbar;
        let refresh = subbutton_with_icon(icons::refresh_s(12), "")
            .on_press_maybe((!self.busy).then_some(ContentMessage::Refresh.into()));
        let toolbar = ContentToolbar {
            leading: crate::menu_renderer::tooltip(
                refresh,
                "Refresh",
                widget::tooltip::Position::Bottom,
            )
            .into(),
            confirm_delete: self.confirm_remove,
            delete: (!locked && count > 0).then_some(
                if self.confirm_remove {
                    ContentMessage::Remove
                } else {
                    ContentMessage::ConfirmRemove
                }
                .into(),
            ),
            all_selected: count > 0 && count == visible.len(),
            select_all: (!self.busy && !visible.is_empty())
                .then_some(ContentMessage::SelectAll.into()),
        };
        let filters = widget::row(CONTENT_TYPES.map(|(kind, label)| {
            crate::menu_renderer::content_filter_button(
                widget::text(label),
                self.installed_kind == kind,
            )
            .on_press_maybe((!self.busy).then_some(ContentMessage::InstalledFilter(kind).into()))
            .into()
        }))
        .spacing(5)
        .wrap();
        body = body.push(toolbar.header(
            None,
            filters.into(),
            count,
            self.installed_search.as_deref(),
            (!self.busy).then_some(|s| ContentMessage::InstalledSearch(s).into()),
        ));
        if visible.is_empty() {
            return body.push(widget::text("No matching packages").style(tsubtitle));
        }
        body.push(widget::responsive(move |size| {
            content_row::installed_list(widget::column(
                visible
                    .iter()
                    .map(|p| self.installed_row(p, locked, size.width)),
            ))
            .into()
        }))
    }

    fn visible_packages(&self) -> impl Iterator<Item = &InstalledPackage> {
        let query = self
            .installed_search
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        self.packages
            .iter()
            .filter(|p| {
                self.installed_kind.is_none_or(|kind| {
                    kind == match p.package.kind {
                        Kind::Mod => ProjectKind::Mod,
                        Kind::Shader | Kind::ShaderPack => ProjectKind::Shader,
                    }
                })
            })
            .filter(move |entry| {
                let p = &entry.package;
                p.filename.to_lowercase().contains(&query)
                    || p.metadata
                        .as_ref()
                        .is_ok_and(|m| m.name.to_lowercase().contains(&query))
            })
    }

    fn installed_row<'a>(
        &self,
        entry: &'a InstalledPackage,
        locked: bool,
        width: f32,
    ) -> Element<'a> {
        let package = &entry.package;
        let kind = package.kind;
        let (name, version, hint) = match &package.metadata {
            Ok(m) => (
                m.name.as_str(),
                m.version.as_str(),
                package.filename.as_str(),
            ),
            Err(error) => (package.filename.as_str(), "Cannot load", error.as_str()),
        };
        let indicator: Element = if kind.is_mod() && package.metadata.is_ok() {
            let filename = package.filename.clone();
            widget::toggler(package.enabled)
                .size(14)
                .on_toggle_maybe((!locked).then_some(move |value| {
                    ContentMessage::Toggle(kind, filename.clone(), value).into()
                }))
                .into()
        } else if package.metadata.is_err() {
            icons::warn_s(16).width(36).into()
        } else {
            widget::text("  S")
                .size(14)
                .color(iced::Color::from_rgb8(0xB8, 0x6E, 0x3C))
                .width(36)
                .into()
        };
        let key = (kind, package.filename.clone());
        let entry = crate::menu_renderer::content_row::ContentRow {
            indicator,
            icon: entry
                .icon
                .as_ref()
                .map_or_else(content_row::missing_icon, |icon| {
                    widget::image(icon.clone())
                        .width(content_row::ICON_SIZE)
                        .height(content_row::ICON_SIZE)
                        .content_fit(iced::ContentFit::ScaleDown)
                        .into()
                }),
            name,
            version,
            name_width: 220.0,
            enabled: !kind.is_mod() || package.enabled,
            selected: self.selected.contains(&key),
            select: ContentMessage::Select(key.0, key.1).into(),
        }
        .button(width);
        crate::menu_renderer::tooltip(entry, hint, widget::tooltip::Position::FollowCursor).into()
    }

    fn registry_filters(&self) -> Column<'_> {
        widget::column(CONTENT_TYPES.map(|(kind, label)| {
            widget::radio(label, kind, Some(self.registry_kind), |kind| {
                ContentMessage::Filter(kind).into()
            })
            .size(12)
            .text_size(14)
            .spacing(5)
            .into()
        }))
    }

    fn can_install(&self, project: &Project) -> bool {
        project.blocked.is_none() && self.installed_package(project).is_none()
    }

    fn installed_package(&self, project: &Project) -> Option<&Package> {
        if self.tab != Tab::Horizon {
            return None;
        }
        self.packages
            .iter()
            .map(|entry| &entry.package)
            .find(|package| project.matches_package(package))
    }

    fn browse<'a>(
        &'a self,
        title: Column<'a>,
        locked: bool,
        images: &'a ImageState,
    ) -> Element<'a> {
        let header = content_store::top_bar(
            (!self.busy).then(|| self.back_message()),
            &self.query,
            (!self.busy).then_some(|s| ContentMessage::Query(s).into()),
            column![widget::text("Horizon registry").size(14)],
        );
        let sidebar = content_store::sidebar(column![
            content_store::type_heading(),
            self.registry_filters()
        ]);
        let mut list = widget::Column::new().padding(5).spacing(5);
        match &self.browse_result {
            BrowseResult::Idle | BrowseResult::Loading => {
                list = list.push(widget::text("Loading catalog…").size(14))
            }
            BrowseResult::Failed(error) => {
                list = list.push(widget::text(error).size(14)).push(
                    widget::button("Retry")
                        .on_press_maybe((!self.busy).then_some(ContentMessage::RetrySearch.into())),
                )
            }
            BrowseResult::Ready(page) => {
                if page.projects.is_empty() {
                    list = list.push(widget::text("No matching projects.").size(14));
                }
                for project in &page.projects {
                    list = list.push(self.project_row(project, images, locked));
                }
            }
        }
        content_store::page(
            header,
            sidebar,
            title.push(content_store::results_list(list, CATALOG_SCROLL, |v| {
                ContentMessage::Scrolled(v).into()
            })),
        )
    }

    fn project_row<'a>(
        &self,
        project: &'a Project,
        images: &ImageState,
        locked: bool,
    ) -> Element<'a> {
        let subtitle = project.blocked.as_deref().unwrap_or(&project.summary);
        let action = if self.installed_package(project).is_some() {
            content_store::uninstall(
                (!locked)
                    .then(|| ContentMessage::Uninstall(project.kind, project.id.clone()).into()),
            )
        } else {
            let (icon, hint) = if let Some(reason) = &project.blocked {
                (icons::warn(), reason.as_str())
            } else {
                (icons::download(), "Install Horizon package")
            };
            crate::menu_renderer::tooltip(
                content_store::action(
                    icon,
                    project.downloads,
                    (!locked && self.can_install(project))
                        .then(|| ContentMessage::Download(project.id.clone()).into()),
                ),
                widget::text!("{} · {hint}", project.kind.label()),
                widget::tooltip::Position::FollowCursor,
            )
            .into()
        };
        content_store::result_row(
            action,
            images.view(project.icon_url.as_deref(), Some(32.0), Some(32.0)),
            &project.name,
            subtitle,
            ContentMessage::OpenProject(project.id.clone()).into(),
        )
    }

    /// Iced emits no scroll events when all loaded rows fit in the viewport.
    pub(crate) fn fill_viewport(&mut self, height: f32) {
        if let (Some(registry), BrowseResult::Ready(results)) =
            (&self.registry, &mut self.browse_result)
        {
            while results.has_next
                && (results.projects.len() as f32 * f32::from(content_store::CARD_HEIGHT)) <= height
            {
                registry.append_next(results, &self.query, self.registry_kind);
            }
        }
    }

    fn search(&mut self, window_height: f32) -> Task<Message> {
        self.scroll = Default::default();
        let scroll = widget::scrollable::snap_to(
            widget::scrollable::Id::new(CATALOG_SCROLL),
            widget::scrollable::RelativeOffset::START,
        );
        if self.tab != Tab::Horizon {
            return Task::none();
        }
        if let Some(registry) = &self.registry {
            self.browse_result =
                BrowseResult::Ready(registry.page(&self.query, self.registry_kind, 0));
            self.fill_viewport(window_height);
            return scroll;
        }
        if self.browse_job.is_some() {
            // Use the latest query when the single registry snapshot arrives.
            return scroll;
        }
        self.browse_job = None;
        self.browse_result = BrowseResult::Loading;
        self.browse_request = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        let request = self.browse_request;
        let fetch = Task::perform(catalog::browse_registry(), move |r| {
            ContentMessage::RegistryLoaded(request, r.strerr()).into()
        });
        let (fetch, handle) = fetch.abortable();
        self.browse_job = Some(handle.abort_on_drop());
        Task::batch([scroll, fetch])
    }

    fn run(&mut self, action: Action) -> Task<Message> {
        let request = self.begin();
        let instance = self.instance.clone();
        Task::perform(
            async move {
                let result: Result<(), ContentError> = async {
                    let config = InstanceConfigJson::read(&instance)
                        .await
                        .map_err(ContentError::other)?;
                    let horizon = config.horizon.ok_or_else(|| {
                        ContentError::other("This is no longer a Horizon instance")
                    })?;
                    match action {
                        Action::Download(id) => {
                            catalog::install_registry(&id, &instance, &horizon).await?;
                        }
                        action => {
                            let dir = instance.get_dot_minecraft_path();
                            tokio::task::spawn_blocking(move || match action {
                                Action::Refresh => Ok(()),
                                Action::Install(paths) => {
                                    paths.iter().try_for_each(|p| content::install(&dir, p))
                                }
                                Action::Toggle(kind, name, value) => {
                                    content::set_enabled(&dir, kind, &name, value)
                                }
                                Action::Remove(packages) => {
                                    packages.into_iter().try_for_each(|(kind, name)| {
                                        content::remove(&dir, kind, &name)
                                    })
                                }
                                _ => unreachable!(),
                            })
                            .await
                            .map_err(ContentError::other)??;
                        }
                    }
                    Ok(())
                }
                .await;
                let mut error = result.err();
                let dir = instance.get_dot_minecraft_path();
                let packages =
                    match tokio::task::spawn_blocking(move || InstalledPackage::load(&dir)).await {
                        Ok(Ok(packages)) => packages,
                        Ok(Err(e)) => {
                            error.get_or_insert_with(|| e.into());
                            Vec::new()
                        }
                        Err(e) => {
                            error.get_or_insert_with(|| ContentError::other(e));
                            Vec::new()
                        }
                    };
                ContentMessage::Finished(request, packages, error)
            },
            Message::from,
        )
    }
}

impl Launcher {
    pub fn update_horizon_content(&mut self, message: ContentMessage) -> Task<Message> {
        let State::HorizonContent(menu) = &mut self.state else {
            return Task::none();
        };
        if self.selected_instance.as_ref() != Some(&menu.instance) {
            return Task::none();
        }
        let running = self.processes.contains_key(&menu.instance);
        match message {
            ContentMessage::DismissError => menu.error = None,
            ContentMessage::Finished(request, packages, error)
                if request == menu.request && menu.busy =>
            {
                menu.busy = false;
                menu.selected.clear();
                menu.packages = packages;
                menu.error = error;
            }
            ContentMessage::RegistryLoaded(request, result)
                if request == menu.browse_request
                    && menu.tab == Tab::Horizon
                    && menu.browse_job.is_some() =>
            {
                menu.browse_job = None;
                match result {
                    Ok(registry) => {
                        menu.registry = Some(registry);
                        return menu.search(self.window_state.size.1);
                    }
                    Err(error) => menu.browse_result = BrowseResult::Failed(error),
                }
            }
            ContentMessage::Picked(request, paths) if request == menu.request && menu.busy => {
                menu.busy = false;
                if paths.is_empty() {
                    return Task::none();
                }
                if running {
                    menu.error = Some(ContentError::other(
                        "Stop Horizon before changing packages.",
                    ));
                } else {
                    return menu.run(Action::Install(paths));
                }
            }
            ContentMessage::Finished(..)
            | ContentMessage::RegistryLoaded(..)
            | ContentMessage::Picked(..) => {}
            ContentMessage::UpdateDescription(request, update) => {
                if let Some(detail) = menu.detail.as_mut().filter(|p| p.request == request)
                    && let Ok(Some(body)) = &mut detail.body
                {
                    body.update(update);
                }
            }
            _ if menu.error.is_some() => {}
            ContentMessage::CloseProject => {
                if menu.detail.take().is_some() {
                    return widget::scrollable::scroll_to(
                        widget::scrollable::Id::new(CATALOG_SCROLL),
                        menu.scroll,
                    );
                }
            }
            _ if menu.busy || menu.detail.is_some() => {}
            ContentMessage::Scrolled(viewport) => {
                if menu.tab == Tab::Horizon {
                    menu.scroll = viewport.absolute_offset();
                    if content_store::needs_continuation(viewport)
                        && let (Some(registry), BrowseResult::Ready(results)) =
                            (&menu.registry, &mut menu.browse_result)
                    {
                        registry.append_next(results, &menu.query, menu.registry_kind);
                    }
                }
            }
            ContentMessage::OpenProject(id) => {
                let BrowseResult::Ready(page) = &menu.browse_result else {
                    return Task::none();
                };
                let Some(project) = page.projects.iter().find(|p| p.id == id) else {
                    return Task::none();
                };
                let request = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
                menu.detail = Some(OpenProject {
                    project: project.clone(),
                    request,
                    body: Ok(Some({
                        frostmark::MarkState::with_html_and_markdown(
                            project
                                .description
                                .as_deref()
                                .filter(|s| !s.trim().is_empty())
                                .unwrap_or(&project.summary),
                        )
                    })),
                    gallery: project
                        .screenshots
                        .iter()
                        .map(|url| ql_mod_manager::store::GalleryItem {
                            url: url.clone(),
                            title: None,
                            description: None,
                        })
                        .collect(),
                });
            }
            ContentMessage::Tab(tab) if tab != menu.tab => {
                menu.tab = tab;
                menu.browse_job = None;
                menu.browse_result = BrowseResult::Idle;
                menu.query.clear();
                menu.selected.clear();
                menu.confirm_remove = false;
                return menu.search(self.window_state.size.1);
            }
            ContentMessage::Query(query) if menu.tab == Tab::Horizon => {
                menu.query = query;
                return menu.search(self.window_state.size.1);
            }
            ContentMessage::Filter(kind) if menu.tab == Tab::Horizon => {
                menu.registry_kind = kind;
                return menu.search(self.window_state.size.1);
            }
            ContentMessage::RetrySearch if menu.tab == Tab::Horizon => {
                return menu.search(self.window_state.size.1);
            }
            ContentMessage::Tab(_)
            | ContentMessage::Query(_)
            | ContentMessage::Filter(_)
            | ContentMessage::RetrySearch => {}
            ContentMessage::InstalledFilter(kind) => {
                menu.selected.clear();
                menu.confirm_remove = false;
                menu.installed_kind = kind;
            }
            ContentMessage::InstalledSearch(query) => {
                menu.selected.clear();
                menu.confirm_remove = false;
                menu.installed_search = query;
            }
            ContentMessage::Select(kind, name) => {
                menu.confirm_remove = false;
                if menu
                    .visible_packages()
                    .any(|p| p.package.kind == kind && p.package.filename == name)
                {
                    let key = (kind, name);
                    if !menu.selected.remove(&key) {
                        menu.selected.insert(key);
                    }
                }
            }
            ContentMessage::SelectAll => {
                menu.confirm_remove = false;
                let visible = menu
                    .visible_packages()
                    .map(|p| (p.package.kind, p.package.filename.clone()))
                    .collect();
                if menu.selected == visible {
                    menu.selected.clear();
                } else {
                    menu.selected = visible;
                }
            }
            ContentMessage::Refresh => return menu.run(Action::Refresh),
            _ if running => {
                menu.error = Some(ContentError::other(
                    "Stop Horizon before changing packages.",
                ));
            }
            ContentMessage::Uninstall(kind, id) => {
                if let BrowseResult::Ready(page) = &menu.browse_result
                    && let Some(project) =
                        page.projects.iter().find(|p| p.id == id && p.kind == kind)
                    && let Some(package) = menu.installed_package(project)
                {
                    let target = (package.kind, package.filename.clone());
                    return menu.run(Action::Remove(vec![target]));
                }
            }
            ContentMessage::Download(id) => {
                if menu.tab == Tab::Horizon
                    && let BrowseResult::Ready(page) = &menu.browse_result
                    && page
                        .projects
                        .iter()
                        .any(|p| p.id == id && menu.can_install(p))
                {
                    return menu.run(Action::Download(id));
                }
            }
            ContentMessage::PickFiles => {
                let request = menu.begin();
                return Task::perform(
                    async move {
                        let dialog = rfd::AsyncFileDialog::new();
                        dialog
                            .set_title("Add Horizon packages")
                            .add_filter("Horizon packages", &["hmod", "hshader", "zip"])
                            .pick_files()
                            .await
                            .into_iter()
                            .flatten()
                            .map(|f| f.path().to_owned())
                            .collect()
                    },
                    move |paths| ContentMessage::Picked(request, paths).into(),
                );
            }
            ContentMessage::Toggle(kind, filename, value) => {
                return menu.run(Action::Toggle(kind, filename, value));
            }
            ContentMessage::ConfirmRemove => menu.confirm_remove = !menu.selected.is_empty(),
            ContentMessage::Remove => {
                if menu.confirm_remove && !menu.selected.is_empty() {
                    let packages = menu.selected.drain().collect();
                    return menu.run(Action::Remove(packages));
                }
            }
        }
        Task::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn launcher() -> Launcher {
        let mut launcher = Launcher::with_error("Could not get launcher dir (test fixture)");
        let instance = Instance::client("horizon");
        launcher.selected_instance = Some(instance.clone());
        launcher.state = State::HorizonContent(Menu::new(instance));
        launcher
    }
    #[test]
    fn stale_results_do_not_change_another_menu() {
        let mut launcher = launcher();
        if let State::HorizonContent(m) = &mut launcher.state {
            m.request = 2;
            m.busy = true;
        }
        let _ = launcher.update_horizon_content(ContentMessage::Finished(
            1,
            vec![],
            Some(ContentError::other("stale")),
        ));
        let _ = launcher.update_horizon_content(ContentMessage::Picked(1, vec![]));
        let _ =
            launcher.update_horizon_content(ContentMessage::RegistryLoaded(1, Err("stale".into())));
        assert!(matches!(&launcher.state, State::HorizonContent(m) if m.busy && m.error.is_none()));
        let _ = launcher.update_horizon_content(ContentMessage::Finished(
            2,
            vec![],
            Some(ContentError::other("current")),
        ));
        assert!(matches!(&launcher.state, State::HorizonContent(m)
            if !m.busy && m.error.as_ref().is_some_and(|e| e.message == "current")));
        let _ = launcher.update_horizon_content(ContentMessage::DismissError);
        launcher.selected_instance = Some(Instance::client("other"));
        let _ = launcher.update_horizon_content(ContentMessage::Refresh);
        assert!(matches!(&launcher.state, State::HorizonContent(m) if !m.busy));
        launcher.state = State::GenericMessage("another screen".into());
        let _ = launcher.update_horizon_content(ContentMessage::Finished(2, vec![], None));
        assert!(matches!(&launcher.state, State::GenericMessage(_)));
    }
    #[test]
    fn native_mod_error_is_dismissed_without_leaving_the_list() {
        use iced::keyboard::{self, Key, key::Named};

        let mut launcher = launcher();
        for key in [None, Some(Named::Escape), Some(Named::Enter)] {
            let State::HorizonContent(menu) = &mut launcher.state else {
                panic!("expected Horizon content");
            };
            menu.installed_search = Some("ore".into());
            let request = menu.begin();
            let _ = launcher.update_horizon_content(ContentMessage::Finished(
                request,
                vec![],
                Some(HorizonError::UnsupportedNativeMod.into()),
            ));
            assert!(matches!(&launcher.state, State::HorizonContent(m)
                if !m.busy && m.error.as_ref().is_some_and(|e| e.title == "Unsupported Horizon mod")));

            let _ = launcher.update_horizon_content(ContentMessage::Tab(Tab::Horizon));
            assert!(matches!(&launcher.state, State::HorizonContent(m) if m.tab == Tab::Installed));

            if let Some(key) = key {
                let _ = launcher.iced_event(
                    iced::Event::Keyboard(keyboard::Event::KeyPressed {
                        key: Key::Named(key),
                        modified_key: Key::Named(key),
                        physical_key: keyboard::key::Physical::Unidentified(
                            keyboard::key::NativeCode::Unidentified,
                        ),
                        location: keyboard::Location::Standard,
                        modifiers: keyboard::Modifiers::empty(),
                        text: None,
                    }),
                    iced::event::Status::Captured,
                );
            } else {
                let _ = launcher.update_horizon_content(ContentMessage::DismissError);
            }
            assert!(matches!(&launcher.state, State::HorizonContent(m)
                if m.tab == Tab::Installed && m.error.is_none() && m.installed_search.as_deref() == Some("ore")));
        }
    }

    #[test]
    fn installed_opens_without_a_catalog_request_and_download_content_starts_one() {
        let mut launcher = launcher();
        assert!(matches!(&launcher.state, State::HorizonContent(m)
            if m.tab == Tab::Installed && m.browse_job.is_none()));
        let _ = launcher.update_horizon_content(ContentMessage::Tab(Tab::Horizon));
        let State::HorizonContent(menu) = &launcher.state else {
            panic!()
        };
        let request = menu.browse_request;
        assert!(matches!(menu.browse_result, BrowseResult::Loading));
        assert!(!menu.busy);
        let _ = launcher.update_horizon_content(ContentMessage::Query("zoom".into()));
        assert!(
            matches!(&launcher.state, State::HorizonContent(m) if m.browse_request == request && m.query == "zoom")
        );
        let _ = launcher.update_horizon_content(ContentMessage::RegistryLoaded(
            request,
            Ok(Registry::default()),
        ));
        let _ = launcher.update_horizon_content(ContentMessage::Filter(Some(ProjectKind::Shader)));
        assert!(matches!(&launcher.state, State::HorizonContent(m)
            if m.registry.is_some() && m.browse_job.is_none() && m.registry_kind == Some(ProjectKind::Shader)
                && matches!(m.browse_result, BrowseResult::Ready(_))));
    }

    #[test]
    fn escape_returns_to_installed_without_clearing_its_search() {
        let mut launcher = launcher();
        let _ =
            launcher.update_horizon_content(ContentMessage::InstalledSearch(Some("ore".into())));
        let _ = launcher.update_horizon_content(ContentMessage::Tab(Tab::Horizon));
        assert!(launcher.key_escape_back(false).0);
        assert!(matches!(&launcher.state, State::HorizonContent(m) if m.tab == Tab::Horizon));
        assert!(launcher.key_escape_back(true).0);
        assert!(
            matches!(&launcher.state, State::HorizonContent(m) if m.tab == Tab::Installed && m.browse_job.is_none() && m.installed_search.as_deref() == Some("ore"))
        );
    }

    #[test]
    fn leaving_registry_ignores_pending_response() {
        let mut launcher = launcher();
        let _ = launcher.update_horizon_content(ContentMessage::Tab(Tab::Horizon));
        let State::HorizonContent(menu) = &launcher.state else {
            unreachable!()
        };
        let old = menu.browse_request;
        let _ = launcher.update_horizon_content(ContentMessage::Tab(Tab::Installed));
        let _ = launcher
            .update_horizon_content(ContentMessage::RegistryLoaded(old, Ok(Registry::default())));
        assert!(
            matches!(&launcher.state, State::HorizonContent(m) if m.registry.is_none() && m.browse_job.is_none())
        );
    }

    fn sky_project() -> Project {
        Project {
            id: "sky".into(),
            name: "Sky".into(),
            kind: ProjectKind::Shader,
            downloads: 0,
            summary: "A sky shader".into(),
            description: Some("# Sky".into()),
            screenshots: Vec::new(),
            icon_url: None,
            blocked: None,
            shaderpack_ids: None,
        }
    }

    #[test]
    fn description_back_preserves_results() {
        let mut launcher = launcher();
        let State::HorizonContent(menu) = &mut launcher.state else {
            unreachable!()
        };
        menu.tab = Tab::Horizon;
        menu.browse_result = BrowseResult::Ready(Page {
            projects: vec![sky_project()],
            has_next: false,
        });
        menu.query = "sky".into();
        menu.scroll.y = 120.0;
        let _ = launcher.update_horizon_content(ContentMessage::OpenProject("sky".into()));
        let State::HorizonContent(menu) = &launcher.state else {
            unreachable!()
        };
        let detail = menu.detail.as_ref().unwrap();
        assert!(matches!(detail.body, Ok(Some(_))));
        assert!(launcher.key_escape_back(true).0);
        let State::HorizonContent(menu) = &mut launcher.state else {
            unreachable!()
        };
        assert!(
            menu.detail.is_none()
                && menu.tab == Tab::Horizon
                && menu.query == "sky"
                && menu.scroll.y == 120.0
        );
    }

    #[test]
    fn registry_uninstall_uses_local_package_identity() {
        let mut launcher = launcher();
        let State::HorizonContent(menu) = &mut launcher.state else {
            unreachable!()
        };
        let project = sky_project();
        menu.packages.push(InstalledPackage {
            icon: None,
            package: Package {
                filename: "custom-name.hshader".into(),
                kind: Kind::Shader,
                enabled: false,
                metadata: Ok(serde_json::from_value(
                    serde_json::json!({"id":"sky", "name":"Sky", "version":"1", "api":"1"}),
                )
                .unwrap()),
            },
        });
        menu.tab = Tab::Horizon;
        assert_eq!(
            menu.installed_package(&project).unwrap().filename,
            "custom-name.hshader"
        );
        menu.browse_result = BrowseResult::Ready(Page {
            projects: vec![project],
            has_next: false,
        });
        let _ = launcher
            .update_horizon_content(ContentMessage::Uninstall(ProjectKind::Mod, "sky".into()));
        assert!(matches!(&launcher.state, State::HorizonContent(m) if !m.busy));
        let _ = launcher
            .update_horizon_content(ContentMessage::Uninstall(ProjectKind::Shader, "sky".into()));
        assert!(
            matches!(&launcher.state, State::HorizonContent(m) if m.busy && m.tab == Tab::Horizon)
        );
    }

    #[test]
    fn removal_requires_confirmation_and_busy_blocks_actions() {
        let mut launcher = launcher();
        if let State::HorizonContent(m) = &mut launcher.state {
            for kind in [Kind::Mod, Kind::Shader] {
                m.packages.push(InstalledPackage {
                    icon: None,
                    package: Package {
                        filename: format!("test.{}", kind.extension()),
                        kind,
                        metadata: Err("fixture".into()),
                        enabled: false,
                    },
                });
            }
        }
        let _ = launcher
            .update_horizon_content(ContentMessage::InstalledFilter(Some(ProjectKind::Shader)));
        let _ = launcher.update_horizon_content(ContentMessage::SelectAll);
        assert!(
            matches!(&launcher.state, State::HorizonContent(m) if m.selected.len()==1 && m.selected.contains(&(Kind::Shader,"test.hshader".into())))
        );
        let _ = launcher.update_horizon_content(ContentMessage::Remove);
        assert!(matches!(&launcher.state, State::HorizonContent(m) if !m.busy));
        let _ = launcher.update_horizon_content(ContentMessage::ConfirmRemove);
        assert!(matches!(&launcher.state, State::HorizonContent(m) if m.confirm_remove));
        let _ = launcher.update_horizon_content(ContentMessage::SelectAll);
        assert!(
            matches!(&launcher.state, State::HorizonContent(m) if !m.confirm_remove && m.selected.is_empty())
        );
        if let State::HorizonContent(m) = &mut launcher.state {
            m.busy = true;
        }
        let _ = launcher.update_horizon_content(ContentMessage::SelectAll);
        assert!(matches!(&launcher.state, State::HorizonContent(m) if m.selected.is_empty()));
    }
}
