//! What the sheet draws and chooses from (step 18.2): the NPC in 3D in the panel *NPC view*, its
//! weapons in its hands; the window choosing a model among all those of the game, each seen in 3D;
//! the items of its hands and its loots, searched by name in every language. The jobs reading them:
//! the GPU's resources, the creatures and items of the database, a figure, a loot, an icon.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use uniwow_api::egui::{self, RichText};
use uniwow_api::viewport::Target;
use uniwow_api::{Context, JobId, JobOutcome, formats, models, theme, vfs, viewport, world_db};

use crate::NpcsModule;
use crate::catalog::{self, Catalog};
use crate::fields;
use crate::figure::{self, Figure, Wanted};
use crate::gpu::{self, Gpu};
use crate::prefab::{self, SLOTS};
use crate::view::NpcView;
use crate::world::{self, Drop, World};

/// The window choosing a model: for which row of the models, the text searched, the kind kept, the
/// display chosen and its figure.
#[derive(Default)]
pub struct Picker {
    row: usize,
    search: String,
    kind: Option<u8>,
    chosen: Option<u32>,
    view: NpcView,
    figure: Option<Figure>,
    job: Option<(JobId, u32)>,
    found: Vec<u32>,
    found_for: Option<(String, Option<u8>)>,
}

#[derive(Default)]
pub struct Stage {
    gpu: Option<Arc<Gpu>>,
    gpu_job: Option<JobId>,
    target: Option<Target>,
    world: Option<Arc<World>>,
    catalog: Option<Arc<Catalog>>,
    /// The server the creatures and items were read for, and the jobs reading them.
    read_for: Option<String>,
    world_job: Option<JobId>,
    catalog_job: Option<JobId>,
    figure: Option<Figure>,
    figure_job: Option<(JobId, Wanted)>,
    /// The figure wanted that could not be drawn, and why.
    refused: Option<(Wanted, String)>,
    view: NpcView,
    /// The model of the prefab shown, by its row.
    pub shown_model: usize,
    pub picker: Option<Picker>,
    loots: HashMap<(usize, u32), Option<Arc<Vec<Drop>>>>,
    loot_jobs: Vec<(JobId, (usize, u32))>,
    icons: HashMap<u32, Option<egui::TextureHandle>>,
    icon_jobs: Vec<(JobId, u32)>,
    icons_wanted: BTreeSet<u32>,
    /// The icons of the spells, by id of `SpellIcon.dbc`.
    spell_icons: HashMap<u32, Option<egui::TextureHandle>>,
    spell_icon_jobs: Vec<(JobId, u32)>,
    spell_icons_wanted: BTreeSet<u32>,
    /// The texts searched among the loots and the items, by slot.
    loot_search: [String; 3],
    item_search: [String; 3],
}

/// A figure read and put on the GPU, from a job.
fn figure_job(
    ctx: &mut Context,
    wanted: Wanted,
    resources: Arc<Gpu>,
    client: Arc<crate::client::Client>,
) -> Option<JobId> {
    let gpu = ctx.gpu().cloned()?;
    let vfs = ctx.service(vfs::SERVICE)?;
    let formats = ctx.service(formats::SERVICE)?;
    let models = ctx.service(models::SERVICE)?;
    Some(
        ctx.spawn("Read an NPC's model and weapons", move |_| -> Result<Figure, String> {
            let (parts, scale, faults) =
                figure::read_parts(&wanted, vfs.as_ref(), formats.as_ref(), models.as_ref(), &client)?;
            Ok(Figure {
                wanted,
                parts: figure::upload(&gpu, &resources, parts),
                scale,
                faults,
            })
        }),
    )
}

impl NpcsModule {
    /// The GPU's resources made once the viewport is there.
    pub(crate) fn start_stage(&mut self, ctx: &mut Context) {
        let Some(view) = ctx.service(viewport::SERVICE) else {
            return;
        };
        let Some(gpu) = ctx.gpu().cloned() else {
            return;
        };
        let target = view.target();
        self.stage.target = Some(target);
        self.stage.gpu_job = Some(ctx.spawn("Build the NPC view's GPU resources", move |_| {
            Arc::new(gpu::create(&gpu.device, &gpu.queue, &target))
        }));
    }

    /// The creatures and the items of the database read once a server.
    fn read_world(&mut self, ctx: &mut Context) {
        let Some(database) = ctx.service(world_db::SERVICE) else {
            return;
        };
        let world_db::State::Connected(server) = database.state() else {
            return;
        };
        if self.stage.read_for.as_ref() == Some(&server) {
            return;
        }
        self.stage.read_for = Some(server);
        let items = database.clone();
        self.stage.world_job = Some(ctx.spawn("Read the creatures of the world database", move |_| {
            World::read(&database).map(Arc::new)
        }));
        self.stage.catalog_job = Some(ctx.spawn("Read the items the NPCs hold", move |_| {
            Catalog::read(&items).map(Arc::new)
        }));
    }

    /// What the prefab opened shows: its model chosen, the weapons of its hands.
    fn wanted(&self) -> Option<Wanted> {
        let prefab = &self.doc.as_ref()?.prefab;
        let model = prefab
            .models
            .get(self.stage.shown_model)
            .or_else(|| prefab.models.first())?;
        let mut hands = [None, None];
        for (at, hand) in hands.iter_mut().enumerate() {
            *hand = self
                .stage
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.get(prefab.equipment[at]))
                .map(|item| (item.display, item.slot));
        }
        Some(Wanted {
            display: model.display,
            hands,
        })
    }

    /// The figure of the prefab read again once what it shows changed.
    fn refigure(&mut self, ctx: &mut Context) {
        let (Some(wanted), Some(resources), Some(client)) =
            (self.wanted(), self.stage.gpu.clone(), self.client.clone())
        else {
            return;
        };
        let current = self.stage.figure.as_ref().map(|figure| &figure.wanted);
        let reading = self.stage.figure_job.as_ref().map(|(_, wanted)| wanted);
        let refused = self.stage.refused.as_ref().map(|(wanted, _)| wanted);
        if [current, reading, refused].contains(&Some(&wanted)) {
            return;
        }
        if let Some(job) = figure_job(ctx, wanted.clone(), resources, client) {
            self.stage.figure_job = Some((job, wanted));
        }
    }

    /// Takes the outcome of a job of the stage; whether it was one.
    pub(crate) fn stage_job(&mut self, job: JobId, outcome: &mut Option<JobOutcome>) -> bool {
        let stage = &mut self.stage;
        let take = |outcome: &mut Option<JobOutcome>| outcome.take().expect("taken once");
        if stage.gpu_job == Some(job) {
            stage.gpu_job = None;
            stage.gpu = take(outcome).take::<Arc<Gpu>>();
        } else if stage.world_job == Some(job) {
            stage.world_job = None;
            match take(outcome).take::<Result<Arc<World>, String>>() {
                Some(Ok(world)) => stage.world = Some(world),
                Some(Err(reason)) => self.said = Some(format!("The creatures are not read: {reason}")),
                None => {}
            }
        } else if stage.catalog_job == Some(job) {
            stage.catalog_job = None;
            match take(outcome).take::<Result<Arc<Catalog>, String>>() {
                Some(Ok(catalog)) => stage.catalog = Some(catalog),
                Some(Err(reason)) => self.said = Some(format!("The items are not read: {reason}")),
                None => {}
            }
        } else if stage.figure_job.as_ref().is_some_and(|(id, _)| *id == job) {
            let (_, wanted) = stage.figure_job.take().expect("matched");
            match take(outcome).take::<Result<Figure, String>>() {
                Some(Ok(figure)) => {
                    stage.refused = None;
                    stage.figure = Some(figure);
                }
                Some(Err(reason)) => stage.refused = Some((wanted, reason)),
                None => stage.refused = Some((wanted, "its job stopped".to_owned())),
            }
        } else if let Some(picker) = stage.picker.as_mut().filter(|p| p.job.is_some_and(|(id, _)| id == job)) {
            picker.job = None;
            picker.figure = take(outcome).take::<Result<Figure, String>>().and_then(Result::ok);
        } else if let Some(at) = stage.loot_jobs.iter().position(|(id, _)| *id == job) {
            let (_, key) = stage.loot_jobs.remove(at);
            let drops = take(outcome).take::<Result<Vec<Drop>, String>>().and_then(Result::ok);
            stage.loots.insert(key, drops.map(Arc::new));
        } else if let Some(at) = stage.spell_icon_jobs.iter().position(|(id, _)| *id == job) {
            let (_, icon) = stage.spell_icon_jobs.remove(at);
            if let (Some(Some((width, height, pixels))), Some(egui)) =
                (take(outcome).take::<Option<catalog::Icon>>(), self.egui.as_ref())
            {
                let image = egui::ColorImage::from_rgba_unmultiplied([width, height], &pixels);
                let texture = egui.load_texture(format!("npcs spell icon {icon}"), image, egui::TextureOptions::LINEAR);
                stage.spell_icons.insert(icon, Some(texture));
            }
        } else if let Some(at) = stage.icon_jobs.iter().position(|(id, _)| *id == job) {
            let (_, display) = stage.icon_jobs.remove(at);
            if let (Some(Some((width, height, pixels))), Some(egui)) =
                (take(outcome).take::<Option<catalog::Icon>>(), self.egui.as_ref())
            {
                let image = egui::ColorImage::from_rgba_unmultiplied([width, height], &pixels);
                let texture = egui.load_texture(format!("npcs icon {display}"), image, egui::TextureOptions::LINEAR);
                stage.icons.insert(display, Some(texture));
            }
        } else {
            return false;
        }
        true
    }

    /// The icons wanted decoded, each by a job, once.
    fn ask_icons(&mut self, ctx: &mut Context) {
        let (Some(client), Some(formats)) = (self.client.clone(), ctx.service(formats::SERVICE)) else {
            return;
        };
        for display in std::mem::take(&mut self.stage.icons_wanted) {
            if self.stage.icons.contains_key(&display) {
                continue;
            }
            self.stage.icons.insert(display, None);
            let Some(name) = client
                .items
                .get(&display)
                .map(|item| item.icon.clone())
                .filter(|n| !n.is_empty())
            else {
                continue;
            };
            let formats = formats.clone();
            let job = ctx.spawn("Decode the icon of an item", move |_| catalog::decode(&formats, &name));
            self.stage.icon_jobs.push((job, display));
        }
    }

    /// The icons of the spells wanted decoded, each by a job, once.
    fn ask_spell_icons(&mut self, ctx: &mut Context) {
        let (Some(client), Some(formats)) = (self.client.clone(), ctx.service(formats::SERVICE)) else {
            return;
        };
        for icon in std::mem::take(&mut self.stage.spell_icons_wanted) {
            if self.stage.spell_icons.contains_key(&icon) {
                continue;
            }
            self.stage.spell_icons.insert(icon, None);
            let Some(path) = client.spell_icons.get(&icon).cloned().filter(|p| !p.is_empty()) else {
                continue;
            };
            let formats = formats.clone();
            let job = ctx.spawn("Decode the icon of a spell", move |_| {
                catalog::decode_path(&formats, &format!("{path}.blp"))
            });
            self.stage.spell_icon_jobs.push((job, icon));
        }
    }

    /// The icon `icon` of `SpellIcon.dbc`, asked when it is not there yet.
    pub(crate) fn spell_icon(&mut self, icon: u32) -> Option<egui::TextureHandle> {
        if icon == 0 {
            return None;
        }
        match self.stage.spell_icons.get(&icon) {
            Some(texture) => texture.clone(),
            None => {
                self.stage.spell_icons_wanted.insert(icon);
                None
            }
        }
    }

    /// The creatures of the database, once read.
    pub(crate) fn world(&self) -> Option<Arc<World>> {
        self.stage.world.clone()
    }

    /// The name of the creature `entry` of the database.
    pub(crate) fn creature_name(&self, entry: u32) -> Option<String> {
        let world = self.stage.world.as_ref()?;
        let at = world
            .creatures
            .binary_search_by_key(&entry, |creature| creature.entry)
            .ok()?;
        Some(world.creatures[at].name().to_owned())
    }

    /// The name of the item `id`, when it is one of the hands.
    pub(crate) fn item_name(&self, id: u32) -> Option<String> {
        self.stage.catalog.as_ref()?.get(id).map(|item| item.name.clone())
    }

    /// The icon of the item display `display`, asked when it is not there yet.
    fn icon(&mut self, display: u32) -> Option<egui::TextureHandle> {
        match self.stage.icons.get(&display) {
            Some(icon) => icon.clone(),
            None => {
                self.stage.icons_wanted.insert(display);
                None
            }
        }
    }

    /// At each frame: the data read, the figure kept to the prefab, the icons decoded.
    pub(crate) fn tick_stage(&mut self, egui: &egui::Context, ctx: &mut Context) {
        self.egui = Some(egui.clone());
        self.read_world(ctx);
        self.refigure(ctx);
        self.ask_icons(ctx);
        self.ask_spell_icons(ctx);
        self.picker_window(egui, ctx);
    }

    /// The panel NPC view: the NPC opened in 3D, its weapons in its hands.
    pub(crate) fn view_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let Some(doc) = self.doc.as_ref() else {
            ui.weak("Open an NPC, or make one: it is shown here in 3D.");
            return;
        };
        let models = doc.prefab.models.clone();
        ui.horizontal_wrapped(|ui| {
            if models.len() > 1 {
                for (at, model) in models.iter().enumerate() {
                    if ui
                        .selectable_label(self.stage.shown_model == at, format!("Model {}", at + 1))
                        .on_hover_text(format!("Shows the display {} among its models", model.display))
                        .clicked()
                    {
                        self.stage.shown_model = at;
                    }
                }
                ui.separator();
            }
            ui.checkbox(&mut self.stage.view.playing, "Breathes")
                .on_hover_text("Plays its Stand, as it stands in the game; held at its start otherwise");
            ui.checkbox(&mut self.stage.view.ground, "Ground")
                .on_hover_text("A neutral ground with a grid of a yard");
            if ui
                .button("Frame")
                .on_hover_text("The whole NPC in view (F over the view)")
                .clicked()
                && let Some(figure) = &self.stage.figure
            {
                self.stage.view.frame(crate::view::height_of(figure));
            }
        });
        if models.is_empty() {
            ui.weak("No model yet: add one in the card Models of the sheet.");
            return;
        }
        if let Some((_, reason)) = &self.stage.refused {
            ui.colored_label(ui.visuals().warn_fg_color, format!("Not drawn: {reason}"));
        }
        if let Some(figure) = &self.stage.figure {
            for fault in &figure.faults {
                ui.colored_label(ui.visuals().warn_fg_color, fault);
            }
        }
        let (Some(resources), Some(gpu), Some(target)) =
            (self.stage.gpu.clone(), ctx.gpu().cloned(), self.stage.target)
        else {
            ui.weak("No GPU: the NPC view is not drawn.");
            return;
        };
        let figure = self
            .stage
            .figure
            .as_ref()
            .filter(|figure| Some(&figure.wanted) == self.wanted().as_ref());
        if figure.is_none() && self.stage.figure_job.is_some() {
            ui.weak("Reading its model…");
        }
        let figure = figure.or(self.stage.figure.as_ref());
        self.stage.view.show(ui, (&gpu, &resources, &target), figure, None);
        ui.weak(
            "drag: turn · middle button: move · wheel: zoom · F: frame · its ranged weapon is drawn when it shoots",
        );
    }

    /// Opens the window choosing the model of the row `row`.
    pub(crate) fn pick_model(&mut self, row: usize) {
        let chosen = self
            .doc
            .as_ref()
            .and_then(|doc| doc.prefab.models.get(row))
            .map(|model| model.display);
        self.stage.picker = Some(Picker {
            row,
            chosen,
            ..Picker::default()
        });
    }

    /// The displays of the client whose id begins with `search`, or whose model's file or one of
    /// whose creatures' names holds it; those of a kind of creature when `kind` is said.
    fn find_displays(&self, search: &str, kind: Option<u8>) -> Vec<u32> {
        let Some(client) = &self.client else {
            return Vec::new();
        };
        let search = search.trim().to_lowercase();
        let world = self.stage.world.as_ref();
        let mut found: Vec<u32> = client
            .models
            .iter()
            .filter(|(display, file)| {
                let wearers = world.and_then(|world| world.wearers.get(display));
                let creatures = || {
                    wearers
                        .into_iter()
                        .flatten()
                        .filter_map(|at| world.and_then(|world| world.creatures.get(*at)))
                };
                if let Some(kind) = kind
                    && !creatures().any(|creature| creature.kind == kind)
                {
                    return false;
                }
                search.is_empty()
                    || display.to_string().starts_with(&search)
                    || file.to_lowercase().contains(&search)
                    || creatures()
                        .any(|creature| creature.names.iter().any(|name| name.to_lowercase().contains(&search)))
            })
            .map(|(display, _)| *display)
            .collect();
        found.sort_unstable();
        found
    }

    /// What wears the display `display`, said in a line.
    fn wearers_said(&self, display: u32, limit: usize) -> String {
        let Some(world) = &self.stage.world else {
            return String::new();
        };
        let names: Vec<String> = world
            .wearers
            .get(&display)
            .into_iter()
            .flatten()
            .filter_map(|at| world.creatures.get(*at))
            .map(|creature| format!("{} ({})", creature.name(), creature.entry))
            .collect();
        match names.len() {
            0 => "worn by no creature of the database".to_owned(),
            count if count > limit => format!("{} and {} more", names[..limit].join(", "), count - limit),
            _ => names.join(", "),
        }
    }

    /// The window choosing a model: the displays searched, the one chosen in 3D; Select or a double
    /// click puts it in its row.
    fn picker_window(&mut self, egui: &egui::Context, ctx: &mut Context) {
        let Some(mut picker) = self.stage.picker.take() else {
            return;
        };
        let mut open = true;
        let mut select = None;
        let key = (picker.search.trim().to_lowercase(), picker.kind);
        if picker.found_for.as_ref() != Some(&key) {
            picker.found = self.find_displays(&key.0, key.1);
            picker.found_for = Some(key);
        }
        egui::Window::new("Choose a model")
            .id(egui::Id::new("npcs picker"))
            .open(&mut open)
            .default_size([900.0, 620.0])
            .show(egui, |ui| {
                let Some(client) = self.client.clone() else {
                    ui.weak("The models are read from the client's CreatureDisplayInfo.dbc: open the client.");
                    return;
                };
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut picker.search)
                            .hint_text("Search: a model's file, an id, a creature's name in any language")
                            .desired_width(360.0),
                    )
                    .on_hover_text("The displays whose id begins with the text, whose model's file holds it, or worn by a creature whose name, in any language, holds it");
                    let kind_name = picker
                        .kind
                        .and_then(|kind| fields::TYPES.iter().find(|(value, _, _)| *value == i64::from(kind)))
                        .map_or("Every kind", |(_, name, _)| *name);
                    egui::ComboBox::from_id_salt("npcs picker kind")
                        .selected_text(kind_name)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(picker.kind.is_none(), "Every kind")
                                .on_hover_text("Every display, worn or not")
                                .clicked()
                            {
                                picker.kind = None;
                            }
                            for (value, name, hint) in &fields::TYPES[1..] {
                                if ui
                                    .selectable_label(picker.kind == Some(*value as u8), *name)
                                    .on_hover_text(format!("The displays worn by a creature of this kind: {hint}"))
                                    .clicked()
                                {
                                    picker.kind = Some(*value as u8);
                                }
                            }
                        })
                        .response
                        .on_hover_text("Keeps the displays worn by the creatures of a kind (their creature_template.type)");
                    ui.label(RichText::new(format!("{} models", picker.found.len())).weak());
                    if self.stage.world.is_none() {
                        ui.label(RichText::new("(the creatures' names come with the world database)").weak().small());
                    }
                });
                ui.separator();
                let height = ui.available_height() - 30.0;
                ui.horizontal(|ui| {
                    let mut clicked = None;
                    let mut doubled = None;
                    ui.vertical(|ui| {
                        ui.set_width(340.0);
                        egui::ScrollArea::vertical()
                            .id_salt("npcs picker list")
                            .max_height(height)
                            .auto_shrink([false, false])
                            .show_rows(ui, 18.0, picker.found.len(), |ui, rows| {
                                for display in &picker.found[rows] {
                                    let file = client.models.get(display).cloned().unwrap_or_default();
                                    let response = ui
                                        .selectable_label(picker.chosen == Some(*display), format!("{display}  {file}"))
                                        .on_hover_text(self.wearers_said(*display, 8));
                                    if response.clicked() {
                                        clicked = Some(*display);
                                    }
                                    if response.double_clicked() {
                                        doubled = Some(*display);
                                    }
                                }
                            });
                    });
                    if let Some(display) = clicked {
                        picker.chosen = Some(display);
                    }
                    if doubled.is_some() {
                        select = doubled;
                    }
                    ui.vertical(|ui| {
                        let Some(display) = picker.chosen else {
                            ui.weak("Click a model to see it here.");
                            return;
                        };
                        ui.label(RichText::new(format!("{display}  {}", client.models.get(&display).cloned().unwrap_or_default())).strong());
                        ui.label(RichText::new(self.wearers_said(display, 4)).weak().small());
                        let wanted = Wanted {
                            display,
                            hands: [None, None],
                        };
                        let shown = picker.figure.as_ref().is_some_and(|figure| figure.wanted == wanted);
                        let reading = picker.job.is_some_and(|(_, held)| held == display);
                        if !shown
                            && !reading
                            && let (Some(resources), Some(client)) = (self.stage.gpu.clone(), self.client.clone())
                            && let Some(job) = figure_job(ctx, wanted, resources, client)
                        {
                            picker.job = Some((job, display));
                        }
                        let (Some(resources), Some(gpu), Some(target)) =
                            (self.stage.gpu.clone(), ctx.gpu().cloned(), self.stage.target)
                        else {
                            ui.weak("No GPU: the model is not drawn.");
                            return;
                        };
                        if !shown {
                            ui.weak("Reading the model…");
                        }
                        let size = egui::vec2(ui.available_width().max(200.0), (height - 40.0).max(160.0));
                        let figure = picker.figure.as_ref().filter(|_| shown);
                        picker.view.show(ui, (&gpu, &resources, &target), figure, Some(size));
                    });
                });
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(picker.chosen.is_some(), egui::Button::new("Select"))
                        .on_hover_text("Gives the NPC this model, in the row the window was opened for")
                        .clicked()
                    {
                        select = picker.chosen;
                    }
                    ui.weak("A double click on a model selects it too · drag the view: turn · wheel: zoom");
                });
            });
        if let Some(display) = select {
            self.set_model_display(picker.row, display, ctx);
            return;
        }
        if open {
            self.stage.picker = Some(picker);
        }
    }

    /// The display of the model of the row `row` made `display`, one undo entry; a new row when it
    /// has none.
    fn set_model_display(&mut self, row: usize, display: u32, ctx: &mut Context) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let mut after = doc.prefab.clone();
        match after.models.get_mut(row) {
            Some(model) => model.display = display,
            None => after.models.push(prefab::Model {
                display,
                scale: 1.0,
                probability: 1.0,
            }),
        }
        self.stage.shown_model = row.min(after.models.len() - 1);
        self.change("Choose a model of the NPC".to_owned(), after, ctx);
    }

    /// The card of its weapons: each slot its item, searched by id or by a name of any language,
    /// with its icon.
    pub(crate) fn weapons_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let equipment = self.doc.as_ref().map(|doc| doc.prefab.equipment).unwrap_or_default();
        theme::section(
            ui,
            egui::Id::new("npcs weapons"),
            theme::icon::STACK,
            "Weapons",
            None,
            true,
            |ui| {
                let Some(catalog) = self.stage.catalog.clone() else {
                    ui.weak("The items are read from the world database: connect it in the panel World database.");
                    return;
                };
                for (at, (_, label, hint)) in SLOTS.iter().enumerate() {
                    let item = equipment[at];
                    let held = catalog.get(item).cloned();
                    ui.horizontal(|ui| {
                    ui.label(RichText::new(*label).weak()).on_hover_text(*hint);
                    if let Some(icon) = held.as_ref().and_then(|held| self.icon(held.display)) {
                        ui.image((icon.id(), egui::vec2(20.0, 20.0)));
                    }
                    let shown = match (&held, item) {
                        (_, 0) => "None".to_owned(),
                        (Some(held), _) => held.name.clone(),
                        (None, _) => format!("{item} (not an item of the hands)"),
                    };
                    let colour = held.as_ref().map(|held| catalog::quality(held.quality));
                    let mut chosen = None;
                    let text = match colour {
                        Some(colour) => RichText::new(shown).color(colour),
                        None => RichText::new(shown),
                    };
                    egui::ComboBox::from_id_salt(("npcs weapon", at))
                        .selected_text(text)
                        .width(240.0)
                        .height(360.0)
                        .show_ui(ui, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.stage.item_search[at])
                                    .hint_text("Search by name, in any language, or id")
                                    .desired_width(240.0),
                            )
                            .on_hover_text("Lists the items of this slot whose name, in any language, holds the text, or whose id begins with it");
                            if ui.selectable_label(item == 0, "None").on_hover_text("An empty hand").clicked() {
                                chosen = Some(0);
                            }
                            let found: Vec<catalog::Item> = catalog
                                .search(at, &self.stage.item_search[at], 200)
                                .into_iter()
                                .cloned()
                                .collect();
                            egui::ScrollArea::vertical().max_height(300.0).show_rows(ui, 22.0, found.len(), |ui, rows| {
                                for entry in &found[rows] {
                                    ui.horizontal(|ui| {
                                        match self.icon(entry.display) {
                                            Some(icon) => {
                                                ui.image((icon.id(), egui::vec2(18.0, 18.0)));
                                            }
                                            None => {
                                                ui.add_space(22.0);
                                            }
                                        }
                                        if ui
                                            .selectable_label(
                                                entry.id == item,
                                                RichText::new(format!("{}  {}", entry.name, entry.id))
                                                    .color(catalog::quality(entry.quality)),
                                            )
                                            .on_hover_text(entry.names.join("\n"))
                                            .clicked()
                                        {
                                            chosen = Some(entry.id);
                                        }
                                    });
                                }
                            });
                        })
                        .response
                        .on_hover_text(format!("{hint}; search by name or id"));
                    ui.label(RichText::new(item.to_string()).weak().small())
                        .on_hover_text("Its entry (item_template.entry)");
                    if let Some(chosen) = chosen.filter(|chosen| *chosen != item)
                        && let Some(doc) = self.doc.as_ref()
                    {
                        let mut after = doc.prefab.clone();
                        after.equipment[at] = chosen;
                        self.stage.item_search[at].clear();
                        self.change(format!("Give the NPC a weapon ({})", label.to_lowercase()), after, ctx);
                    }
                });
                }
                ui.label(
                    RichText::new("Its armour is its model's: a weapon alone is given here (creature_equip_template).")
                        .weak()
                        .small(),
                );
            },
        );
    }

    /// The loot of kind `kind` (0 kill, 1 pickpocket, 2 skinning): its creatures searched by name
    /// in any language or by entry, the loot chosen given; its contents under the pointer.
    pub(crate) fn loot_field(&mut self, ui: &mut egui::Ui, kind: usize, ctx: &mut Context) {
        let (table, column) = world::LOOTS[kind];
        let label = fields::LOOT[kind].label;
        let hint = fields::LOOT[kind].hint;
        let loot = self.doc.as_ref().map_or(0, |doc| doc.prefab.whole(column)) as u32;
        let Some(world) = self.stage.world.clone() else {
            ui.label(RichText::new(label).weak()).on_hover_text(hint);
            ui.label(loot.to_string());
            ui.label(RichText::new("(the loots come with the world database)").weak().small());
            ui.end_row();
            return;
        };
        ui.label(RichText::new(label).weak()).on_hover_text(hint);
        let owners: Vec<String> = world
            .owners_of(kind, loot)
            .take(3)
            .map(|creature| format!("{} ({})", creature.name(), creature.entry))
            .collect();
        let shown = match (loot, owners.is_empty()) {
            (0, _) => "None".to_owned(),
            (_, true) => format!("{loot} (its own)"),
            _ => format!("That of {}", owners.join(", ")),
        };
        let mut chosen = None;
        let combo = egui::ComboBox::from_id_salt(("npcs loot", kind))
            .selected_text(shown)
            .width(280.0)
            .height(360.0)
            .show_ui(ui, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.stage.loot_search[kind])
                        .hint_text("A creature whose loot it takes: name or entry")
                        .desired_width(260.0),
                )
                .on_hover_text("A loot has no name of its own: the creatures that leave one are listed, searched by a name in any language or by entry; the NPC takes the loot of the one chosen");
                if ui.selectable_label(loot == 0, "None").on_hover_text("It leaves nothing").clicked() {
                    chosen = Some(0);
                }
                let found: Vec<world::Creature> = world
                    .loot_owners(kind, &self.stage.loot_search[kind], 200)
                    .into_iter()
                    .cloned()
                    .collect();
                egui::ScrollArea::vertical().max_height(300.0).show_rows(ui, 18.0, found.len(), |ui, rows| {
                    for creature in &found[rows] {
                        let own = creature.loots[kind];
                        let response = ui.selectable_label(
                            own == loot,
                            format!("{}  {}  → loot {own}", creature.name(), creature.entry),
                        );
                        let response = response.on_hover_ui(|ui| self.loot_contents(ui, kind, own, ctx));
                        if response.clicked() {
                            chosen = Some(own);
                        }
                    }
                });
            });
        let response = combo.response;
        if loot != 0 {
            response.on_hover_ui(|ui| self.loot_contents(ui, kind, loot, ctx));
        } else {
            response.on_hover_text(format!("{hint}; search a creature whose loot it takes"));
        }
        ui.label(RichText::new(loot.to_string()).weak().small())
            .on_hover_text(format!("Its entry in {table} (creature_template.{column})"));
        ui.end_row();
        if let Some(chosen) = chosen.filter(|chosen| *chosen != loot)
            && let Some(doc) = self.doc.as_ref()
        {
            let mut after = doc.prefab.clone();
            after
                .template
                .insert(column.to_owned(), uniwow_api::serde_json::json!(chosen));
            self.stage.loot_search[kind].clear();
            self.change(format!("Set the {} of the NPC", label.to_lowercase()), after, ctx);
        }
    }

    /// What the loot `loot` of kind `kind` holds: each item, its icon, name and chance; read by a
    /// job the first time.
    fn loot_contents(&mut self, ui: &mut egui::Ui, kind: usize, loot: u32, ctx: &mut Context) {
        let key = (kind, loot);
        match self.stage.loots.get(&key).cloned() {
            None => {
                if !self.stage.loot_jobs.iter().any(|(_, asked)| *asked == key)
                    && let Some(database) = ctx.service(world_db::SERVICE)
                {
                    let table = world::LOOTS[kind].0;
                    let job = ctx.spawn("Read a loot", move |_| world::read_loot(&database, table, loot));
                    self.stage.loot_jobs.push((job, key));
                }
                ui.weak("Reading the loot…");
            }
            Some(None) => {
                ui.weak("The loot is not read.");
            }
            Some(Some(drops)) => {
                ui.label(RichText::new(format!("Loot {loot}: {} lines", drops.len())).strong());
                for drop in drops.iter().take(24) {
                    ui.horizontal(|ui| {
                        match self.icon(drop.display) {
                            Some(icon) => {
                                ui.image((icon.id(), egui::vec2(16.0, 16.0)));
                            }
                            None => {
                                ui.add_space(20.0);
                            }
                        }
                        let what = if drop.reference != 0 {
                            RichText::new(format!("the loot {} it refers to", drop.reference)).italics()
                        } else {
                            RichText::new(if drop.name.is_empty() {
                                format!("item {}", drop.item)
                            } else {
                                drop.name.clone()
                            })
                            .color(catalog::quality(drop.quality))
                        };
                        ui.label(what);
                        let counts = match drop.counts {
                            (low, high) if low == high && low <= 1 => String::new(),
                            (low, high) if low == high => format!(" ×{low}"),
                            (low, high) => format!(" ×{low}-{high}"),
                        };
                        let chance = if drop.chance == 0.0 {
                            "in a group".to_owned()
                        } else {
                            format!("{:.1} %", drop.chance.abs())
                        };
                        ui.label(RichText::new(format!("{chance}{counts}")).weak());
                    });
                }
                if drops.len() > 24 {
                    ui.weak(format!("and {} more", drops.len() - 24));
                }
            }
        }
    }
}
