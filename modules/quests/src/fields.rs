//! The masks, flags and choices of a quest (as the user asked it): never a number to type, but
//! boxes of elements to tick and named lists, each element saying what it does in the game; the
//! races and classes, the types and sorts of quests, the skills and the zones named by the client's
//! tables, the custom ones of a patched client with them; the raw value folded, read only.

use uniwow_api::egui::{self, RichText};
use uniwow_api::serde_json::json;
use uniwow_api::{Context, theme};

use crate::{QuestsModule, data, shown};

/// An element of a box or a list: its value (a bit, or the value chosen), its name, what it does.
pub type Element = (i64, String, String);

/// The categories of `SkillLine.dbc` a quest asks: the professions and the secondary skills.
const PROFESSIONS: [u32; 2] = [11, 9];

impl QuestsModule {
    fn whole(&self, id: u32, table: &str, column: &str) -> i64 {
        self.value(id, table, "", column)
            .as_ref()
            .and_then(data::as_number)
            .map_or(0, |value| value as i64)
    }

    fn write_whole(&mut self, id: u32, table: &str, column: &str, value: i64, label: &str, ctx: &mut Context) {
        let mut after = self.edits.clone();
        after.set(id, table, "", column, json!(value));
        self.change(format!("Set the {label} of the quest {id}"), after, ctx);
    }

    /// The raw value of `column`, folded, read only.
    fn raw_ui(ui: &mut egui::Ui, salt: &str, column: &str, value: i64) {
        egui::CollapsingHeader::new(RichText::new("Raw value").weak().small())
            .id_salt(("quests raw", salt))
            .default_open(false)
            .show(ui, |ui| {
                ui.label(RichText::new(format!("{column} = {value}")).monospace().weak())
                    .on_hover_text("As the database keeps it, read only");
            });
    }

    /// A box of the bits of `column`: each ticked or not, its name and what it does; `none` what 0
    /// means, said.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mask_ui(
        &mut self,
        ui: &mut egui::Ui,
        id: u32,
        (table, column): (&str, &str),
        elements: &[Element],
        (label, none): (&str, &str),
        ctx: &mut Context,
    ) {
        let value = self.whole(id, table, column);
        ui.label(RichText::new(label).strong())
            .on_hover_text(format!("{none} ({table}.{column})"));
        if value == 0 {
            ui.label(RichText::new(none).italics().weak());
        }
        let mut changed = None;
        egui::Grid::new(("quests mask", column))
            .num_columns(3)
            .spacing([12.0, 2.0])
            .show(ui, |ui| {
                for (at, (bit, name, hint)) in elements.iter().enumerate() {
                    let mut ticked = value & bit != 0;
                    if ui
                        .checkbox(&mut ticked, name)
                        .on_hover_text(format!("{hint} (bit {bit} of {column})"))
                        .changed()
                    {
                        changed = Some(if ticked { value | bit } else { value & !bit });
                    }
                    if at % 3 == 2 {
                        ui.end_row();
                    }
                }
            });
        Self::raw_ui(ui, column, column, value);
        if let Some(value) = changed {
            self.write_whole(id, table, column, value, label, ctx);
        }
    }

    /// A list of the values of `column`, each named, what it does under the pointer; a value no
    /// element has shown as it is.
    pub(crate) fn choice_ui(
        &mut self,
        ui: &mut egui::Ui,
        id: u32,
        (table, column): (&str, &str),
        elements: &[Element],
        (label, hint): (&str, &str),
        ctx: &mut Context,
    ) {
        let value = self.whole(id, table, column);
        let mut chosen = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).strong())
                .on_hover_text(format!("{hint} ({table}.{column})"));
            let shown = elements
                .iter()
                .find(|(held, _, _)| *held == value)
                .map_or(format!("{value}"), |(_, name, _)| name.clone());
            egui::ComboBox::from_id_salt(("quests choice", column))
                .selected_text(shown)
                .height(320.0)
                .show_ui(ui, |ui| {
                    for (held, name, element) in elements {
                        if ui
                            .selectable_label(*held == value, name)
                            .on_hover_text(format!("{element} ({column} = {held})"))
                            .clicked()
                        {
                            chosen = Some(*held);
                        }
                    }
                })
                .response
                .on_hover_text(format!("{hint} ({table}.{column})"));
        });
        if let Some(value) = chosen.filter(|chosen| *chosen != value) {
            self.write_whole(id, table, column, value, label, ctx);
        }
    }

    /// The card of who takes the quest: its type, the way it is taken, where it is listed, the
    /// races, classes and skill it asks.
    pub(crate) fn who_ui(&mut self, ui: &mut egui::Ui, id: u32, ctx: &mut Context) {
        let client = self.client.clone().unwrap_or_default();
        theme::section(
            ui,
            egui::Id::new("quests who"),
            theme::icon::INFO,
            "Who takes it",
            None,
            true,
            |ui| {
                let mut infos: Vec<Element> =
                    vec![(0, "Normal".to_owned(), "No type shown beside the title".to_owned())];
                infos.extend(client.infos.iter().map(|(info, name)| {
                    (
                        i64::from(*info),
                        name.clone(),
                        format!("Shown as {name} beside the title in the log"),
                    )
                }));
                self.choice_ui(
                    ui,
                    id,
                    ("quest_template", "QuestInfoID"),
                    &infos,
                    (
                        "Type",
                        "The type shown beside the title in the quest log, of QuestInfo.dbc",
                    ),
                    ctx,
                );
                let ways: Vec<Element> = data::QUEST_TYPES
                    .iter()
                    .map(|(value, name, hint)| (*value, (*name).to_owned(), (*hint).to_owned()))
                    .collect();
                self.choice_ui(
                    ui,
                    id,
                    ("quest_template", "QuestType"),
                    &ways,
                    (
                        "Taken",
                        "How the quest is taken: done the usual way, completed at once, or never offered",
                    ),
                    ctx,
                );
                // Where it is listed: a sort of QuestSort.dbc below 0, a zone above.
                let mut sorts: Vec<Element> =
                    vec![(0, "Nowhere".to_owned(), "Listed under no zone nor sort".to_owned())];
                sorts.extend(client.sorts.iter().map(|(sort, name)| {
                    (
                        -i64::from(*sort),
                        format!("{name} (sort)"),
                        format!("Listed in the log under {name}, a sort of QuestSort.dbc"),
                    )
                }));
                sorts.extend(client.zones.iter().map(|(zone, name)| {
                    (
                        i64::from(*zone),
                        name.clone(),
                        format!("Listed in the log under the zone {name}, of AreaTable.dbc"),
                    )
                }));
                self.choice_ui(
                    ui,
                    id,
                    ("quest_template", "QuestSortID"),
                    &sorts,
                    (
                        "Listed under",
                        "The heading the quest is listed under in the log: a zone, or a sort as Epic or a profession",
                    ),
                    ctx,
                );
                ui.separator();
                let races: Vec<Element> = client
                    .races
                    .iter()
                    .filter(|(race, _)| (1..=32).contains(race))
                    .map(|(race, name)| {
                        (
                            1i64 << (race - 1),
                            name.clone(),
                            format!("A {name} can take it (race {race})"),
                        )
                    })
                    .collect();
                self.mask_ui(
                    ui,
                    id,
                    ("quest_template", "AllowableRaces"),
                    &races,
                    ("Races", "None ticked: every race can take it"),
                    ctx,
                );
                ui.separator();
                let classes: Vec<Element> = client
                    .classes
                    .iter()
                    .filter(|(class, _)| (1..=32).contains(class))
                    .map(|(class, name)| {
                        (
                            1i64 << (class - 1),
                            name.clone(),
                            format!("A {name} can take it (class {class})"),
                        )
                    })
                    .collect();
                self.mask_ui(
                    ui,
                    id,
                    ("quest_template_addon", "AllowableClasses"),
                    &classes,
                    ("Classes", "None ticked: every class can take it"),
                    ctx,
                );
                if client.races.is_empty() || client.classes.is_empty() {
                    ui.label(
                    RichText::new("The races and classes are read from the client's ChrRaces.dbc and ChrClasses.dbc: open the client.")
                        .weak()
                        .small(),
                );
                }
                ui.separator();
                let mut skills: Vec<Element> = vec![(0, "None".to_owned(), "No skill asked".to_owned())];
                skills.extend(
                    client
                        .skills
                        .iter()
                        .filter(|(_, category, _)| PROFESSIONS.contains(category))
                        .map(|(skill, _, name)| {
                            (
                                i64::from(*skill),
                                name.clone(),
                                format!("Only a player with {name} can take it (skill {skill})"),
                            )
                        }),
                );
                self.choice_ui(
                    ui,
                    id,
                    ("quest_template_addon", "RequiredSkillID"),
                    &skills,
                    (
                        "Skill",
                        "A profession or secondary skill the player must have, of SkillLine.dbc",
                    ),
                    ctx,
                );
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Skill points").strong()).on_hover_text(
                        "The points of that skill the player must have (quest_template_addon.RequiredSkillPoints)",
                    );
                    let current = shown(
                        self.value(id, "quest_template_addon", "", "RequiredSkillPoints")
                            .as_ref(),
                    );
                    let key = format!("{id}:quest_template_addon::RequiredSkillPoints");
                    if let Some(typed) = self.typed(
                        ui,
                        key,
                        &current,
                        false,
                        60.0,
                        "The points of the skill asked (quest_template_addon.RequiredSkillPoints)",
                    ) {
                        self.write_number(
                            id,
                            "quest_template_addon",
                            "RequiredSkillPoints",
                            "skill points",
                            &typed,
                            ctx,
                        );
                    }
                });
            },
        );
    }

    /// The card of the flags: those of the quest the client is sent, and the server's own.
    pub(crate) fn flags_ui(&mut self, ui: &mut egui::Ui, id: u32, ctx: &mut Context) {
        theme::section(
            ui,
            egui::Id::new("quests flags"),
            theme::icon::SLIDERS,
            "Flags",
            None,
            false,
            |ui| {
                let flags: Vec<Element> = data::QUEST_FLAGS
                    .iter()
                    .map(|flag| (i64::from(flag.bit), flag.name.to_owned(), flag.hint.to_owned()))
                    .collect();
                self.mask_ui(
                    ui,
                    id,
                    ("quest_template", "Flags"),
                    &flags,
                    ("Of the quest", "None ticked: a quest done once, the usual way"),
                    ctx,
                );
                ui.separator();
                let special: Vec<Element> = data::SPECIAL_FLAGS
                    .iter()
                    .map(|flag| (i64::from(flag.bit), flag.name.to_owned(), flag.hint.to_owned()))
                    .collect();
                self.mask_ui(
                    ui,
                    id,
                    ("quest_template_addon", "SpecialFlags"),
                    &special,
                    ("Of the server", "None ticked: no flag of the server's own"),
                    ctx,
                );
            },
        );
    }
}
