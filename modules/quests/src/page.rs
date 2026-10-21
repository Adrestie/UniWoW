//! The page of a quest (step 16.3, as the user asked it after): drawn as the client's quest log
//! shows a quest, its fields where they stand: the title, the description, the objectives and what
//! they ask (creatures or game objects ×N, items to bring ×N), the rewards (the money in gold,
//! silver and copper, the items received and those to choose from, the reputations, a spell, a
//! title); each list of slots ended by an empty "+" that opens a search by id or by name in every
//! language; the texts of the giver and ender, the requirements and chain, the givers and enders
//! in cards under the page. The data and the application are those of the module.

use uniwow_api::Context;
use uniwow_api::egui::{self, Color32, RichText};
use uniwow_api::serde_json::{Value, json};

use crate::catalog::{self, Kind};
use crate::{QuestsModule, data, shown};

/// A list of slots of a quest: what it is, its columns of ids and counts, what it holds (an item,
/// or a creature, a game object below 0), what it says.
pub struct Slots {
    pub label: &'static str,
    pub ids: &'static [&'static str],
    pub counts: &'static [&'static str],
    pub kind: Kind,
    pub hint: &'static str,
}

pub const KILL: Slots = Slots {
    label: "Kill or use",
    ids: &[
        "RequiredNpcOrGo1",
        "RequiredNpcOrGo2",
        "RequiredNpcOrGo3",
        "RequiredNpcOrGo4",
    ],
    counts: &[
        "RequiredNpcOrGoCount1",
        "RequiredNpcOrGoCount2",
        "RequiredNpcOrGoCount3",
        "RequiredNpcOrGoCount4",
    ],
    kind: Kind::Creature,
    hint: "A creature to kill, or a game object to use, so many times; 4 at most",
};

pub const BRING: Slots = Slots {
    label: "Bring",
    ids: &[
        "RequiredItemId1",
        "RequiredItemId2",
        "RequiredItemId3",
        "RequiredItemId4",
        "RequiredItemId5",
        "RequiredItemId6",
    ],
    counts: &[
        "RequiredItemCount1",
        "RequiredItemCount2",
        "RequiredItemCount3",
        "RequiredItemCount4",
        "RequiredItemCount5",
        "RequiredItemCount6",
    ],
    kind: Kind::Item,
    hint: "An item the player brings back, so many; taken when the quest is handed in; 6 at most",
};

pub const RECEIVE: Slots = Slots {
    label: "You will receive",
    ids: &["RewardItem1", "RewardItem2", "RewardItem3", "RewardItem4"],
    counts: &["RewardAmount1", "RewardAmount2", "RewardAmount3", "RewardAmount4"],
    kind: Kind::Item,
    hint: "An item given with the quest, so many; 4 at most",
};

pub const CHOOSE: Slots = Slots {
    label: "You will be able to choose one of these rewards",
    ids: &[
        "RewardChoiceItemID1",
        "RewardChoiceItemID2",
        "RewardChoiceItemID3",
        "RewardChoiceItemID4",
        "RewardChoiceItemID5",
        "RewardChoiceItemID6",
    ],
    counts: &[
        "RewardChoiceItemQuantity1",
        "RewardChoiceItemQuantity2",
        "RewardChoiceItemQuantity3",
        "RewardChoiceItemQuantity4",
        "RewardChoiceItemQuantity5",
        "RewardChoiceItemQuantity6",
    ],
    kind: Kind::Item,
    hint: "One item of these the player chooses, so many; 6 at most",
};

/// The reputations: their factions and values.
const FACTIONS: [&str; 5] = [
    "RewardFactionID1",
    "RewardFactionID2",
    "RewardFactionID3",
    "RewardFactionID4",
    "RewardFactionID5",
];
const VALUES: [&str; 5] = [
    "RewardFactionValue1",
    "RewardFactionValue2",
    "RewardFactionValue3",
    "RewardFactionValue4",
    "RewardFactionValue5",
];

const QUEST: &str = "quest_template";

/// The colours of the page: the parchment, its ink, its headings, the fields on it.
const PARCHMENT: Color32 = Color32::from_rgb(232, 218, 186);
const INK: Color32 = Color32::from_rgb(43, 28, 12);
const HEADING: Color32 = Color32::from_rgb(110, 40, 10);
const FIELD: Color32 = Color32::from_rgb(244, 236, 214);
const COINS: [(Color32, &str, &str); 3] = [
    (Color32::from_rgb(212, 175, 55), "g", "gold"),
    (Color32::from_rgb(192, 192, 196), "s", "silver"),
    (Color32::from_rgb(184, 115, 51), "c", "copper"),
];

/// The edge of the fields and slots on the parchment, and the ink of its hints.
const EDGE: Color32 = Color32::from_rgb(120, 85, 45);
const HINT: Color32 = Color32::from_rgb(92, 74, 52);

/// The style of the page alone, the rest of the editor in its theme: the ink dark on the
/// parchment, the fields cream with a thin brown edge, the cursor and the selection seen.
fn parchment_style(ui: &mut egui::Ui) {
    let visuals = ui.visuals_mut();
    visuals.override_text_color = Some(INK);
    visuals.weak_text_color = Some(HINT);
    visuals.text_edit_bg_color = Some(FIELD);
    visuals.extreme_bg_color = FIELD;
    visuals.faint_bg_color = FIELD;
    visuals.code_bg_color = FIELD;
    visuals.hyperlink_color = HEADING;
    visuals.selection.bg_fill = Color32::from_rgb(214, 180, 112);
    visuals.selection.stroke = egui::Stroke::new(1.0, INK);
    visuals.text_cursor.stroke = egui::Stroke::new(2.0, INK);
    let fills = [
        (&mut visuals.widgets.noninteractive, FIELD, 1.0),
        (&mut visuals.widgets.inactive, FIELD, 1.0),
        (&mut visuals.widgets.hovered, Color32::from_rgb(250, 244, 226), 1.5),
        (&mut visuals.widgets.active, Color32::from_rgb(236, 224, 196), 1.5),
        (&mut visuals.widgets.open, FIELD, 1.0),
    ];
    for (widget, fill, width) in fills {
        widget.bg_fill = fill;
        widget.weak_bg_fill = fill;
        widget.bg_stroke = egui::Stroke::new(width, EDGE);
        widget.fg_stroke = egui::Stroke::new(1.0, INK);
    }
    // The page itself: no edge drawn around its plain labels.
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, EDGE.gamma_multiply(0.5));
}

/// A label of the page, as its headings are: dark brown, strong.
fn subheading(text: &str) -> RichText {
    RichText::new(text).strong().color(HEADING)
}

/// The search opened by a slot "+": the slots it fills, what it searches, the text typed.
pub struct Search {
    pub quest: u32,
    pub slots: &'static Slots,
    pub kind: Kind,
    pub text: String,
}

fn whole(value: Option<&Value>) -> i64 {
    value.and_then(data::as_number).map_or(0, |value| value as i64)
}

/// The gold, silver and copper of `copper`.
pub fn coins(copper: i64) -> [i64; 3] {
    let copper = copper.abs();
    [copper / 10_000, copper / 100 % 100, copper % 100]
}

impl QuestsModule {
    /// The filled slots of `slots` of the quest `id`, in their order: the id (below 0 a game object)
    /// and the count.
    pub(crate) fn slots_of(&self, id: u32, slots: &Slots) -> Vec<(i64, i64)> {
        slots
            .ids
            .iter()
            .zip(slots.counts)
            .map(|(held, count)| {
                (
                    whole(self.value(id, QUEST, "", held).as_ref()),
                    whole(self.value(id, QUEST, "", count).as_ref()),
                )
            })
            .filter(|(held, _)| *held != 0)
            .collect()
    }

    /// The slots of `slots` of the quest `id` made `filled`, the others emptied: one undo entry.
    pub(crate) fn write_slots(
        &mut self,
        id: u32,
        slots: &Slots,
        filled: &[(i64, i64)],
        label: String,
        ctx: &mut Context,
    ) {
        let mut after = self.edits.clone();
        for (at, (held, count)) in slots.ids.iter().zip(slots.counts).enumerate() {
            let (value, amount) = filled.get(at).copied().unwrap_or((0, 0));
            after.set(id, QUEST, "", held, json!(value));
            after.set(id, QUEST, "", count, json!(amount));
        }
        self.change(label, after, ctx);
    }

    /// A field of the page: typed on the parchment, written once left.
    fn page_field(
        &mut self,
        ui: &mut egui::Ui,
        id: u32,
        (table, locale, column): (&str, &str, &str),
        lines: bool,
        (hint, label): (&str, &str),
        ctx: &mut Context,
    ) {
        let current = shown(self.value(id, table, locale, column).as_ref());
        let key = format!("{id}:{table}:{locale}:{column}");
        let width = ui.available_width();
        let said = format!("{hint} ({table}.{column})");
        if let Some(typed) = self.typed(ui, key, &current, lines, width, &said) {
            let mut after = self.edits.clone();
            after.set(id, table, locale, column, json!(typed));
            self.change(format!("Write the {label} of the quest {id}"), after, ctx);
        }
        if current.is_empty() {
            ui.label(RichText::new(hint).italics().small().color(HINT));
        }
    }

    /// A number of the page, typed, written once left.
    fn page_number(
        &mut self,
        ui: &mut egui::Ui,
        id: u32,
        column: &str,
        width: f32,
        (label, hint): (&str, &str),
        ctx: &mut Context,
    ) {
        let current = shown(self.value(id, QUEST, "", column).as_ref());
        let key = format!("{id}:{QUEST}::{column}");
        if let Some(typed) = self.typed(ui, key, &current, false, width, &format!("{hint} ({column})")) {
            self.write_number(id, QUEST, column, label, &typed, ctx);
        }
    }

    /// The text of the language chosen: its table, locale and column.
    fn localized(&self, text: &data::Text) -> (&'static str, &'static str, &'static str) {
        match data::LOCALES[self.locale] {
            "enUS" => (text.table, "", text.column),
            locale => (text.locale_table, locale, text.locale_column),
        }
    }

    /// The page of the quest `id`.
    pub(crate) fn page_ui(&mut self, ui: &mut egui::Ui, id: u32, ctx: &mut Context) {
        let frame = egui::Frame::new()
            .fill(PARCHMENT)
            .stroke(egui::Stroke::new(1.0, Color32::from_rgb(150, 120, 80)))
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::same(16));
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            parchment_style(ui);
            let heading = |ui: &mut egui::Ui, text: &str, hint: &str| {
                ui.add_space(6.0);
                ui.label(RichText::new(text).size(17.0).strong().color(HEADING))
                    .on_hover_text(hint);
            };
            // The title, large.
            let title = self.localized(&data::TEXTS[0]);
            ui.scope(|ui| {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(20.0));
                self.page_field(ui, id, title, false, ("The title of the quest", "title"), ctx);
            });
            heading(
                ui,
                "Description",
                "What the giver tells, offering the quest: QuestDescription",
            );
            let text = self.localized(&data::TEXTS[1]);
            self.page_field(
                ui,
                id,
                text,
                true,
                (
                    "What the giver says. $B a new line, $N the player's name",
                    "description",
                ),
                ctx,
            );
            heading(
                ui,
                "Objectives",
                "The objectives in a sentence, in the quest log: LogDescription",
            );
            let text = self.localized(&data::TEXTS[2]);
            self.page_field(ui, id, text, true, ("Kill 10 Mana Wyrms…", "objectives"), ctx);
            self.kill_ui(ui, id, ctx);
            self.items_ui(ui, id, &BRING, ctx);

            heading(ui, "Rewards", "What the player is given once the quest is handed in");
            self.money_ui(ui, id, ctx);
            ui.horizontal(|ui| {
                ui.label(subheading("Experience (difficulty)")).on_hover_text(
                    "RewardXPDifficulty: 0 to 9, the experience given grows with it and with the level of the quest",
                );
                self.page_number(ui, id, "RewardXPDifficulty", 40.0, ("experience", "0 to 9"), ctx);
            });
            self.items_ui(ui, id, &RECEIVE, ctx);
            self.items_ui(ui, id, &CHOOSE, ctx);
            self.reputations_ui(ui, id, ctx);
            ui.horizontal(|ui| {
                ui.label(subheading("Spell cast"))
                    .on_hover_text("RewardSpell: the id of a spell cast on the player, 0 for none");
                self.page_number(ui, id, "RewardSpell", 70.0, ("spell", "The id of a spell"), ctx);
                ui.label(subheading("Title"))
                    .on_hover_text("RewardTitle: the id of a title of CharTitles.dbc, 0 for none");
                self.page_number(ui, id, "RewardTitle", 50.0, ("title", "The id of a title"), ctx);
            });
        });
    }

    /// The creatures or game objects to kill or use, each its count and its line of the log.
    fn kill_ui(&mut self, ui: &mut egui::Ui, id: u32, ctx: &mut Context) {
        let filled = self.slots_of(id, &KILL);
        let mut removed = None;
        for (at, (held, count)) in filled.iter().enumerate() {
            let (kind, entry) = if *held < 0 {
                (Kind::GameObject, -*held)
            } else {
                (Kind::Creature, *held)
            };
            let name = self
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.get(kind, entry as u32))
                .map_or(format!("{} {entry}", kind.name()), |found| found.name.clone());
            ui.horizontal(|ui| {
                ui.label(RichText::new(name.as_str()).strong()).on_hover_text(format!(
                    "The {} {entry}; {}",
                    kind.name(),
                    KILL.hint
                ));
                ui.label("×");
                let key = format!("{id}:{QUEST}::{}", KILL.counts[at]);
                let hint = format!("How many times to kill or use it ({})", KILL.counts[at]);
                if let Some(typed) = self.typed(ui, key, &count.to_string(), false, 50.0, &hint) {
                    let mut changed = filled.clone();
                    changed[at].1 = typed.trim().parse().unwrap_or(1);
                    self.write_slots(id, &KILL, &changed, format!("Set the count of the quest {id}"), ctx);
                }
                if ui
                    .small_button("✖")
                    .on_hover_text("Takes it out of the objectives")
                    .clicked()
                {
                    removed = Some(at);
                }
            });
            // Its line of the quest log.
            let place = self.localized(&data::TEXTS[7 + at.min(3)]);
            ui.indent(("kill text", at), |ui| {
                self.page_field(
                    ui,
                    id,
                    place,
                    false,
                    (
                        "Its line in the log, else the creature's name: ObjectiveText",
                        "objective",
                    ),
                    ctx,
                );
            });
        }
        if let Some(at) = removed {
            let mut changed = filled.clone();
            changed.remove(at);
            self.write_slots(id, &KILL, &changed, format!("Take an objective of the quest {id}"), ctx);
        }
        if filled.len() < KILL.ids.len()
            && ui
                .button("+ A creature or game object to kill or use")
                .on_hover_text(KILL.hint)
                .clicked()
        {
            self.searching = Some(Search {
                quest: id,
                slots: &KILL,
                kind: Kind::Creature,
                text: String::new(),
            });
        }
    }

    /// A list of item slots: each its icon, count and ✖, then a "+" while one is free.
    fn items_ui(&mut self, ui: &mut egui::Ui, id: u32, slots: &'static Slots, ctx: &mut Context) {
        let filled = self.slots_of(id, slots);
        ui.label(subheading(&format!("{}:", slots.label)))
            .on_hover_text(slots.hint);
        let mut removed = None;
        let mut counted = None;
        ui.horizontal_wrapped(|ui| {
            for (at, (item, count)) in filled.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.set_width(56.0);
                    self.item_slot(ui, *item as u32);
                    ui.horizontal(|ui| {
                        let key = format!("{id}:{QUEST}::{}", slots.counts[at]);
                        let hint = format!("How many of it ({})", slots.counts[at]);
                        if let Some(typed) = self.typed(ui, key, &count.to_string(), false, 28.0, &hint) {
                            counted = Some((at, typed.trim().parse().unwrap_or(1)));
                        }
                        if ui.small_button("✖").on_hover_text("Takes it out").clicked() {
                            removed = Some(at);
                        }
                    });
                });
            }
            if filled.len() < slots.ids.len() {
                let (rect, response) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::click());
                let lit = response.hovered();
                ui.painter().rect(
                    rect,
                    egui::CornerRadius::same(4),
                    if lit { Color32::from_rgb(250, 244, 226) } else { FIELD },
                    egui::Stroke::new(1.5, EDGE),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "+",
                    egui::FontId::proportional(22.0),
                    INK,
                );
                if response.on_hover_text(format!("Add: {}", slots.hint)).clicked() {
                    self.searching = Some(Search {
                        quest: id,
                        slots,
                        kind: Kind::Item,
                        text: String::new(),
                    });
                }
            }
        });
        if let Some((at, count)) = counted {
            let mut changed = filled.clone();
            changed[at].1 = count;
            self.write_slots(id, slots, &changed, format!("Set a count of the quest {id}"), ctx);
        }
        if let Some(at) = removed {
            let mut changed = filled.clone();
            changed.remove(at);
            self.write_slots(id, slots, &changed, format!("Take an item of the quest {id}"), ctx);
        }
    }

    /// The slot of the item `item`: its icon, edged in the colour of its quality; its name, id and
    /// quality under the pointer.
    fn item_slot(&mut self, ui: &mut egui::Ui, item: u32) {
        let entry = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.get(Kind::Item, item).cloned());
        let (rect, response) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
        let colour = entry
            .as_ref()
            .map_or(Color32::GRAY, |entry| catalog::quality(entry.quality));
        let painter = ui.painter();
        painter.rect_filled(rect, egui::CornerRadius::same(4), Color32::from_rgb(30, 24, 18));
        match entry.as_ref().and_then(|entry| self.icon(entry.display)) {
            Some(texture) => {
                painter.image(
                    texture.id(),
                    rect.shrink(2.0),
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                let short: String = entry
                    .as_ref()
                    .map_or("?".to_owned(), |entry| entry.name.chars().take(3).collect());
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    short,
                    egui::FontId::proportional(12.0),
                    colour,
                );
            }
        }
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(4),
            egui::Stroke::new(2.0, colour),
            egui::StrokeKind::Inside,
        );
        let said = match &entry {
            Some(entry) => format!("{} ({})", entry.name, entry.id),
            None => format!("item {item}"),
        };
        response.on_hover_ui(|ui| {
            ui.label(RichText::new(said).color(colour));
        });
    }

    /// The money given (or asked, below 0) in gold, silver and copper.
    fn money_ui(&mut self, ui: &mut egui::Ui, id: u32, ctx: &mut Context) {
        let copper = whole(self.value(id, QUEST, "", "RewardMoney").as_ref());
        let parts = coins(copper);
        let mut written = None;
        ui.horizontal(|ui| {
            ui.label(subheading(if copper < 0 { "Money asked" } else { "Money" })).on_hover_text(
                "RewardMoney, in copper: 1 gold is 100 silver, 1 silver 100 copper; below 0 the money the quest asks",
            );
            for (at, (colour, letter, name)) in COINS.iter().enumerate() {
                let key = format!("{id}:money:{letter}");
                let hint =
                    format!("The {name} of the money, shown to the player in the rewards (RewardMoney, in copper)");
                if let Some(typed) = self.typed(ui, key, &parts[at].to_string(), false, 44.0, &hint) {
                    let mut changed = parts;
                    changed[at] = typed.trim().parse().unwrap_or(0).max(0);
                    let total = changed[0] * 10_000 + changed[1] * 100 + changed[2];
                    written = Some(if copper < 0 { -total } else { total });
                }
                let (rect, response) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 5.5, *colour);
                response.on_hover_text(*name);
            }
        });
        if let Some(total) = written {
            let mut after = self.edits.clone();
            after.set(id, QUEST, "", "RewardMoney", json!(total));
            self.change(format!("Set the money of the quest {id}"), after, ctx);
        }
    }

    /// The reputations given: each its faction and value, one row empty to fill while one is free.
    fn reputations_ui(&mut self, ui: &mut egui::Ui, id: u32, ctx: &mut Context) {
        ui.label(subheading("Reputation:")).on_hover_text(
            "RewardFactionID and RewardFactionValue: a faction of Faction.dbc and what it is given; 5 at most",
        );
        let mut shown_empty = false;
        for (faction, value) in FACTIONS.iter().zip(VALUES) {
            let held = whole(self.value(id, QUEST, "", faction).as_ref());
            if held == 0 {
                if shown_empty {
                    continue;
                }
                shown_empty = true;
            }
            ui.horizontal(|ui| {
                ui.label(subheading("faction"));
                self.page_number(
                    ui,
                    id,
                    faction,
                    60.0,
                    ("faction", "The id of a faction of Faction.dbc"),
                    ctx,
                );
                ui.label(subheading("value"));
                self.page_number(
                    ui,
                    id,
                    value,
                    60.0,
                    ("reputation", "The reputation given, below 0 taken"),
                    ctx,
                );
            });
        }
    }

    /// The window of the search opened by a "+": by id or by a name of any language, among what the
    /// catalog holds; a click fills the next slot.
    pub(crate) fn search_ui(&mut self, egui_ctx: &egui::Context, ctx: &mut Context) {
        let Some(search) = &mut self.searching else {
            return;
        };
        let (mut open, mut chosen) = (true, None);
        let title = format!("Choose: {}", search.slots.label.to_lowercase());
        egui::Window::new(title)
            .id(egui::Id::new("quests search"))
            .open(&mut open)
            .default_size([420.0, 460.0])
            .show(egui_ctx, |ui| {
                if search.slots.kind != Kind::Item {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut search.kind, Kind::Creature, "Creatures")
                            .on_hover_text("Among creature_template");
                        ui.selectable_value(&mut search.kind, Kind::GameObject, "Game objects")
                            .on_hover_text("Among gameobject_template");
                    });
                }
                let field = ui
                    .add(
                        egui::TextEdit::singleline(&mut search.text)
                            .hint_text("An id, or a name in any language")
                            .desired_width(f32::INFINITY),
                    )
                    .on_hover_text("Searches by id, or by a part of a name in any language of the client; read once from the database");
                field.request_focus();
                let Some(catalog) = self.catalog.clone() else {
                    ui.label("Reading the items, creatures and game objects of the database…");
                    return;
                };
                let found = catalog.search(search.kind, &search.text, 200);
                ui.label(RichText::new(format!("{} shown", found.len())).weak().small());
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    for entry in found {
                        let colour = if search.kind == Kind::Item {
                            catalog::quality(entry.quality)
                        } else {
                            ui.visuals().text_color()
                        };
                        let row = ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{:>6}", entry.id)).monospace().weak());
                            ui.label(RichText::new(&entry.name).color(colour));
                        });
                        let response = row.response.interact(egui::Sense::click());
                        if response.hovered() {
                            ui.painter().rect_filled(
                                row.response.rect,
                                egui::CornerRadius::same(2),
                                ui.visuals().widgets.hovered.weak_bg_fill.gamma_multiply(0.4),
                            );
                        }
                        if response.on_hover_text("Chosen by a click").clicked() {
                            chosen = Some(entry.id);
                        }
                    }
                });
            });
        let Some(search) = self.searching.take() else {
            return;
        };
        if let Some(entry) = chosen {
            let value = if search.kind == Kind::GameObject {
                -i64::from(entry)
            } else {
                i64::from(entry)
            };
            let mut filled = self.slots_of(search.quest, search.slots);
            if filled.len() < search.slots.ids.len() {
                filled.push((value, 1));
                self.write_slots(
                    search.quest,
                    search.slots,
                    &filled,
                    format!("Add a {} to the quest {}", search.kind.name(), search.quest),
                    ctx,
                );
            }
        } else if open {
            self.searching = Some(search);
        }
    }

    /// The icon of the display `display`, once decoded; asked of a job meanwhile.
    fn icon(&mut self, display: u32) -> Option<egui::TextureHandle> {
        if let Some(icon) = self.icons.get(&display) {
            return icon.clone();
        }
        self.icons_wanted.insert(display);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contrast of `a` on `b`, as WCAG 2 counts it: 4.5 at least for a text to read.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let light = |colour: Color32| {
            let channel = |value: u8| {
                let value = f32::from(value) / 255.0;
                if value <= 0.039_28 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(colour.r()) + 0.7152 * channel(colour.g()) + 0.0722 * channel(colour.b())
        };
        let (a, b) = (light(a), light(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn every_text_of_the_page_reads_on_the_parchment_and_in_its_fields() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            parchment_style(ui);
            let visuals = ui.visuals();
            assert_eq!(
                visuals.text_edit_bg_color,
                Some(FIELD),
                "the fields cream, not of the dark theme"
            );
            for widget in [
                &visuals.widgets.inactive,
                &visuals.widgets.hovered,
                &visuals.widgets.active,
            ] {
                assert!(contrast(widget.fg_stroke.color, widget.bg_fill) >= 7.0);
            }
        });
        output.textures_delta.clear();
        for background in [PARCHMENT, FIELD] {
            assert!(contrast(INK, background) >= 7.0, "the ink");
            assert!(contrast(HEADING, background) >= 4.5, "the headings and labels");
            assert!(contrast(HINT, background) >= 4.5, "the hints");
        }
        assert!(contrast(EDGE, FIELD) >= 3.0, "the edge of a field or a slot");
    }

    #[test]
    fn the_money_is_shown_in_gold_silver_and_copper() {
        assert_eq!(coins(100_000_000), [10_000, 0, 0]);
        assert_eq!(coins(12_345), [1, 23, 45]);
        assert_eq!(coins(-250), [0, 2, 50], "the money asked as much");
    }
}
