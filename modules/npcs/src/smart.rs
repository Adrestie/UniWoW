//! What a skill of an NPC is (step 18.3): a line of `smart_scripts` (source 0, its entry the NPC's),
//! an event, an action and a target, each of the kinds AzerothCore's SmartScriptMgr.h runs, every
//! parameter named and said, never a number to guess. Those of the client of another expansion
//! (scenes, anim kits) and those AzerothCore marks as not supported are left out.

use uniwow_api::serde_json::{Value, json};

/// What a parameter is: how it is chosen and shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// A number, its unit said ("ms", "yd", "%", "" for none).
    Number(&'static str),
    /// Yes or no, 1 or 0.
    Bool,
    /// One of named values.
    Choice(&'static [(i64, &'static str)]),
    /// A mask of named bits.
    Mask(&'static [(i64, &'static str)]),
    Spell,
    Creature,
    GameObject,
    Item,
    Quest,
    Emote,
    /// A group of the NPC's texts (`creature_text.GroupID`).
    Text,
    /// A phase of its script, 1 to 12.
    Phase,
    /// Phases of its script, a mask.
    Phases,
    Faction,
    Display,
}

/// A parameter: its name, what it is, how it is chosen.
#[derive(Clone, Copy, Debug)]
pub struct Param {
    pub name: &'static str,
    pub hint: &'static str,
    pub kind: Kind,
}

const fn p(name: &'static str, hint: &'static str, kind: Kind) -> Param {
    Param { name, hint, kind }
}

/// An event, an action or a target: its id, its name, what it does, its parameters in order.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub id: i64,
    pub name: &'static str,
    pub hint: &'static str,
    pub params: &'static [Param],
}

const fn s(id: i64, name: &'static str, hint: &'static str, params: &'static [Param]) -> Spec {
    Spec { id, name, hint, params }
}

const MS: Kind = Kind::Number("ms");
const YD: Kind = Kind::Number("yd");
const PCT: Kind = Kind::Number("%");
const N: Kind = Kind::Number("");

const INITIAL_MIN: Param = p(
    "first after, from",
    "The first time, after at least these milliseconds",
    MS,
);
const INITIAL_MAX: Param = p("to", "The first time, after at most these milliseconds", MS);
const REPEAT_MIN: Param = p(
    "then every, from",
    "Then again every so many milliseconds at least; 0 once",
    MS,
);
const REPEAT_MAX: Param = p("to", "Then again every so many milliseconds at most", MS);
const COOLDOWN_MIN: Param = p(
    "not again before",
    "Milliseconds before it can happen again, at least",
    MS,
);
const COOLDOWN_MAX: Param = p("to", "Milliseconds before it can happen again, at most", MS);
const TIMED: [Param; 4] = [INITIAL_MIN, INITIAL_MAX, REPEAT_MIN, REPEAT_MAX];

const HOSTILITY: &[(i64, &str)] = &[(0, "hostile"), (1, "not hostile"), (2, "anyone")];
const SCHOOLS: &[(i64, &str)] = &[
    (1, "Physical"),
    (2, "Holy"),
    (4, "Fire"),
    (8, "Nature"),
    (16, "Frost"),
    (32, "Shadow"),
    (64, "Arcane"),
];

/// The events of a creature (`SMART_EVENT`).
pub const EVENTS: &[Spec] = &[
    s(
        0,
        "In combat, every…",
        "Again and again while it fights (SMART_EVENT_UPDATE_IC)",
        &TIMED,
    ),
    s(
        1,
        "Out of combat, every…",
        "Again and again while it does not fight (SMART_EVENT_UPDATE_OOC)",
        &TIMED,
    ),
    s(
        60,
        "Always, every…",
        "Again and again, fighting or not (SMART_EVENT_UPDATE)",
        &TIMED,
    ),
    s(
        2,
        "Its health between",
        "When its health is between two percentages, again and again (SMART_EVENT_HEALTH_PCT)",
        &[
            p("from", "The lowest health, in percent", PCT),
            p("to", "The highest health, in percent", PCT),
            REPEAT_MIN,
            REPEAT_MAX,
        ],
    ),
    s(
        3,
        "Its mana between",
        "When its mana is between two percentages (SMART_EVENT_MANA_PCT)",
        &[
            p("from", "The lowest mana, in percent", PCT),
            p("to", "The highest mana, in percent", PCT),
            REPEAT_MIN,
            REPEAT_MAX,
        ],
    ),
    s(4, "On aggro", "When it starts to fight (SMART_EVENT_AGGRO)", &[]),
    s(
        5,
        "On a kill",
        "When it kills (SMART_EVENT_KILL)",
        &[
            COOLDOWN_MIN,
            COOLDOWN_MAX,
            p("players alone", "Only when it kills a player", Kind::Bool),
            p(
                "creature",
                "Only when it kills this creature; none for any",
                Kind::Creature,
            ),
        ],
    ),
    s(6, "On death", "When it dies (SMART_EVENT_DEATH)", &[]),
    s(
        7,
        "On evade",
        "When it leaves the fight and goes home (SMART_EVENT_EVADE)",
        &[],
    ),
    s(
        21,
        "Back home",
        "When it is back at its place after a fight (SMART_EVENT_REACHED_HOME)",
        &[],
    ),
    s(
        25,
        "On reset",
        "After a fight, when it respawns and when it appears (SMART_EVENT_RESET)",
        &[],
    ),
    s(
        11,
        "On respawn",
        "When it respawns (SMART_EVENT_RESPAWN)",
        &[
            p(
                "where",
                "Anywhere, or only on a map or in a zone",
                Kind::Choice(&[(0, "anywhere"), (1, "on the map"), (2, "in the zone")]),
            ),
            p("map", "The map, when on a map", N),
            p("zone", "The zone, when in a zone", N),
        ],
    ),
    s(37, "AI started", "When its script starts (SMART_EVENT_AI_INIT)", &[]),
    s(
        63,
        "Just created",
        "When it is just created (SMART_EVENT_JUST_CREATED)",
        &[],
    ),
    s(
        54,
        "Just summoned",
        "When it is just summoned by another (SMART_EVENT_JUST_SUMMONED)",
        &[],
    ),
    s(
        8,
        "Hit by a spell",
        "When a spell hits it (SMART_EVENT_SPELLHIT)",
        &[
            p("spell", "Only this spell; none for any", Kind::Spell),
            p(
                "school",
                "Only spells of these schools; none for any",
                Kind::Mask(SCHOOLS),
            ),
            COOLDOWN_MIN,
            COOLDOWN_MAX,
        ],
    ),
    s(
        31,
        "Its spell hits",
        "When a spell of its own hits its target (SMART_EVENT_SPELLHIT_TARGET)",
        &[
            p("spell", "Only this spell; none for any", Kind::Spell),
            p(
                "school",
                "Only spells of these schools; none for any",
                Kind::Mask(SCHOOLS),
            ),
            COOLDOWN_MIN,
            COOLDOWN_MAX,
        ],
    ),
    s(
        32,
        "Damaged",
        "When it takes damage (SMART_EVENT_DAMAGED)",
        &[
            p("from", "The least damage of the blow", N),
            p("to", "The most damage of the blow", N),
            COOLDOWN_MIN,
            COOLDOWN_MAX,
        ],
    ),
    s(
        53,
        "Healed",
        "When it is healed (SMART_EVENT_RECEIVE_HEAL)",
        &[
            p("from", "The least heal", N),
            p("to", "The most heal", N),
            COOLDOWN_MIN,
            COOLDOWN_MAX,
        ],
    ),
    s(
        9,
        "Its target within range",
        "When its target is within two distances (SMART_EVENT_RANGE)",
        &[
            p("from", "The nearest", YD),
            p("to", "The farthest", YD),
            REPEAT_MIN,
            REPEAT_MAX,
        ],
    ),
    s(
        110,
        "In melee range",
        "When its target is within a distance, or not (SMART_EVENT_IS_IN_MELEE_RANGE)",
        &[
            INITIAL_MIN,
            INITIAL_MAX,
            REPEAT_MIN,
            REPEAT_MAX,
            p("distance", "The distance; 0 for that of melee", YD),
            p("not", "When its target is NOT within it", Kind::Bool),
        ],
    ),
    s(
        12,
        "Its target's health between",
        "When its target's health is between two percentages (SMART_EVENT_TARGET_HEALTH_PCT)",
        &[
            p("from", "In percent", PCT),
            p("to", "In percent", PCT),
            REPEAT_MIN,
            REPEAT_MAX,
        ],
    ),
    s(
        18,
        "Its target's mana between",
        "When its target's mana is between two percentages (SMART_EVENT_TARGET_MANA_PCT)",
        &[
            p("from", "In percent", PCT),
            p("to", "In percent", PCT),
            REPEAT_MIN,
            REPEAT_MAX,
        ],
    ),
    s(
        13,
        "Its target casts",
        "When its target is casting (SMART_EVENT_VICTIM_CASTING)",
        &[
            REPEAT_MIN,
            REPEAT_MAX,
            p("spell", "Only this spell; none for any", Kind::Spell),
        ],
    ),
    s(
        67,
        "Behind its target",
        "When it stands behind its target (SMART_EVENT_IS_BEHIND_TARGET)",
        &[INITIAL_MIN, INITIAL_MAX, REPEAT_MIN, REPEAT_MAX],
    ),
    s(
        23,
        "Has an aura",
        "When it has an aura, so many stacks (SMART_EVENT_HAS_AURA)",
        &[
            p("spell", "The aura", Kind::Spell),
            p("stacks", "How many at least", N),
            REPEAT_MIN,
            REPEAT_MAX,
        ],
    ),
    s(
        24,
        "Its target has an aura",
        "When its target has an aura (SMART_EVENT_TARGET_BUFFED)",
        &[
            p("spell", "The aura", Kind::Spell),
            p("stacks", "How many at least", N),
            REPEAT_MIN,
            REPEAT_MAX,
        ],
    ),
    s(
        14,
        "An ally is hurt",
        "When an ally near it misses so much health (SMART_EVENT_FRIENDLY_HEALTH)",
        &[
            p("health missing", "The health the ally misses, at least", N),
            p("within", "How far the ally is", YD),
            REPEAT_MIN,
            REPEAT_MAX,
        ],
    ),
    s(
        74,
        "An ally's health under",
        "When an ally near it is under a percentage of its health (SMART_EVENT_FRIENDLY_HEALTH_PCT)",
        &[
            INITIAL_MIN,
            INITIAL_MAX,
            REPEAT_MIN,
            REPEAT_MAX,
            p("health", "Under this percentage", PCT),
            p("within", "How far the ally is", YD),
        ],
    ),
    s(
        15,
        "An ally is controlled",
        "When an ally near it is stunned, feared, charmed… (SMART_EVENT_FRIENDLY_IS_CC)",
        &[p("within", "How far the ally is", YD), REPEAT_MIN, REPEAT_MAX],
    ),
    s(
        16,
        "An ally misses an aura",
        "When an ally near it lacks an aura (SMART_EVENT_FRIENDLY_MISSING_BUFF)",
        &[
            p("spell", "The aura", Kind::Spell),
            p("within", "How far the ally is", YD),
            REPEAT_MIN,
            REPEAT_MAX,
            p("in combat alone", "Only while it fights", Kind::Bool),
        ],
    ),
    s(
        10,
        "Sees someone (not fighting)",
        "When someone comes into its sight while it does not fight (SMART_EVENT_OOC_LOS)",
        &[
            p("who", "Who it reacts to", Kind::Choice(HOSTILITY)),
            p("within", "How far it sees", YD),
            COOLDOWN_MIN,
            COOLDOWN_MAX,
            p("players alone", "Only players", Kind::Bool),
        ],
    ),
    s(
        26,
        "Sees someone (fighting)",
        "When someone comes into its sight while it fights (SMART_EVENT_IC_LOS)",
        &[
            p("who", "Who it reacts to", Kind::Choice(HOSTILITY)),
            p("within", "How far it sees", YD),
            COOLDOWN_MIN,
            COOLDOWN_MAX,
            p("players alone", "Only players", Kind::Bool),
        ],
    ),
    s(
        101,
        "Players near",
        "When at least so many players are near (SMART_EVENT_NEAR_PLAYERS)",
        &[
            p("at least", "How many players", N),
            p("within", "How near", YD),
            p("first after", "Milliseconds before it is checked first", MS),
            REPEAT_MIN,
            REPEAT_MAX,
        ],
    ),
    s(
        102,
        "Few players near",
        "When at most so many players are near (SMART_EVENT_NEAR_PLAYERS_NEGATION)",
        &[
            p("at most", "How many players", N),
            p("within", "How near", YD),
            p("first after", "Milliseconds before it is checked first", MS),
            REPEAT_MIN,
            REPEAT_MAX,
        ],
    ),
    s(
        22,
        "Receives an emote",
        "When a player makes an emote at it (SMART_EVENT_RECEIVE_EMOTE)",
        &[p("emote", "The emote", Kind::Emote), COOLDOWN_MIN, COOLDOWN_MAX],
    ),
    s(
        17,
        "Summons a creature",
        "When it summons a creature (SMART_EVENT_SUMMONED_UNIT)",
        &[
            p("creature", "Only this one; none for any", Kind::Creature),
            COOLDOWN_MIN,
            COOLDOWN_MAX,
        ],
    ),
    s(
        82,
        "Its summon dies",
        "When a creature it summoned dies (SMART_EVENT_SUMMONED_UNIT_DIES)",
        &[
            p("creature", "Only this one; none for any", Kind::Creature),
            COOLDOWN_MIN,
            COOLDOWN_MAX,
        ],
    ),
    s(
        35,
        "Its summon despawns",
        "When a creature it summoned disappears (SMART_EVENT_SUMMON_DESPAWNED)",
        &[
            p("creature", "Only this one; none for any", Kind::Creature),
            COOLDOWN_MIN,
            COOLDOWN_MAX,
        ],
    ),
    s(
        19,
        "A quest is taken from it",
        "When a player takes a quest it gives (SMART_EVENT_ACCEPTED_QUEST)",
        &[
            p("quest", "Only this quest; none for any", Kind::Quest),
            COOLDOWN_MIN,
            COOLDOWN_MAX,
        ],
    ),
    s(
        20,
        "A quest is handed in",
        "When a player hands in a quest to it (SMART_EVENT_REWARD_QUEST)",
        &[
            p("quest", "Only this quest; none for any", Kind::Quest),
            COOLDOWN_MIN,
            COOLDOWN_MAX,
        ],
    ),
    s(
        64,
        "Spoken to",
        "When a player opens its gossip (SMART_EVENT_GOSSIP_HELLO)",
        &[p(
            "when",
            "Its gossip or its use",
            Kind::Choice(&[(0, "always"), (1, "gossip alone"), (2, "use alone")]),
        )],
    ),
    s(
        62,
        "Gossip option chosen",
        "When a player chooses an option of its gossip (SMART_EVENT_GOSSIP_SELECT)",
        &[
            p("menu", "The gossip menu (gossip_menu.MenuID)", N),
            p("option", "The option (gossip_menu_option.OptionID)", N),
        ],
    ),
    s(
        52,
        "A text is over",
        "When a text said is over (SMART_EVENT_TEXT_OVER)",
        &[
            p("text", "The text said", Kind::Text),
            p("creature", "The creature that said it; none for itself", Kind::Creature),
        ],
    ),
    s(
        59,
        "Timed event",
        "When a timed event of its own comes (SMART_EVENT_TIMED_EVENT_TRIGGERED)",
        &[p("event", "The id of the timed event (see Create a timed event)", N)],
    ),
    s(
        66,
        "Phase changes",
        "When its script enters one of these phases (SMART_EVENT_EVENT_PHASE_CHANGE)",
        &[p("phases", "The phases", Kind::Phases)],
    ),
    s(
        38,
        "Data set",
        "When another sets a datum on it (SMART_EVENT_DATA_SET)",
        &[
            p("field", "The field", N),
            p("value", "The value", N),
            COOLDOWN_MIN,
            COOLDOWN_MAX,
        ],
    ),
    s(
        77,
        "Counter reached",
        "When a counter of its own reaches a value (SMART_EVENT_COUNTER_SET)",
        &[
            p("counter", "The counter", N),
            p("value", "The value", N),
            COOLDOWN_MIN,
            COOLDOWN_MAX,
        ],
    ),
    s(
        34,
        "Movement ends",
        "When it arrives where it was sent (SMART_EVENT_MOVEMENTINFORM)",
        &[
            p("movement", "The kind of movement; 0 for any", N),
            p("point", "The point it reached", N),
        ],
    ),
    s(
        108,
        "Waypoint reached",
        "When it reaches a point of its path (SMART_EVENT_WAYPOINT_REACHED)",
        &[
            p("point", "The point; 0 for any", N),
            p("path", "The path; 0 for any", N),
        ],
    ),
    s(
        109,
        "Path ended",
        "When it reaches the end of its path (SMART_EVENT_WAYPOINT_ENDED)",
        &[
            p("point", "The point; 0 for any", N),
            p("path", "The path; 0 for any", N),
        ],
    ),
    s(
        36,
        "Corpse removed",
        "When its corpse disappears (SMART_EVENT_CORPSE_REMOVED)",
        &[],
    ),
];

const CAST_FLAGS: &[(i64, &str)] = &[
    (1, "interrupt the current cast"),
    (2, "triggered (no cast time, no cost)"),
    (32, "only if the target has not the aura"),
    (64, "combat movement stops while it casts"),
    (128, "only when it is not casting"),
];
const SUMMON_TYPES: &[(i64, &str)] = &[
    (1, "despawns after the time, or when out of combat"),
    (2, "despawns after the time"),
    (3, "despawns after the time out of combat"),
    (4, "despawns after the time once dead"),
    (5, "despawns after the time once its corpse is gone"),
    (6, "despawns when its corpse is gone"),
    (7, "despawns when dead"),
    (8, "never despawns by itself"),
];
const SHEATHS: &[(i64, &str)] = &[
    (0, "weapons away"),
    (1, "melee weapon drawn"),
    (2, "ranged weapon drawn"),
];
const REACT: &[(i64, &str)] = &[(0, "passive"), (1, "defensive"), (2, "aggressive")];
const STANDS: &[(i64, &str)] = &[(1, "sitting"), (3, "sleeping"), (7, "dead"), (8, "kneeling")];
const POWERS: &[(i64, &str)] = &[(0, "mana"), (1, "rage"), (3, "energy"), (6, "runic power")];

/// The actions (`SMART_ACTION`).
pub const ACTIONS: &[Spec] = &[
    s(
        11,
        "Cast a spell",
        "It casts a spell on the target (SMART_ACTION_CAST)",
        &[
            p("spell", "The spell", Kind::Spell),
            p("how", "How it casts", Kind::Mask(CAST_FLAGS)),
            p("trigger flags", "Raw TriggerCastFlags; 0 for none", N),
            p("targets at most", "On so many of the targets at most; 0 for all", N),
        ],
    ),
    s(
        85,
        "Make the target cast on itself",
        "Each target casts the spell on itself (SMART_ACTION_SELF_CAST)",
        &[
            p("spell", "The spell", Kind::Spell),
            p("how", "How it casts", Kind::Mask(CAST_FLAGS)),
            p("trigger flags", "Raw TriggerCastFlags; 0 for none", N),
            p("targets at most", "On so many of the targets at most; 0 for all", N),
        ],
    ),
    s(
        134,
        "Make the invoker cast",
        "The one who caused the event casts the spell on the target (SMART_ACTION_INVOKER_CAST)",
        &[
            p("spell", "The spell", Kind::Spell),
            p("how", "How it casts", Kind::Mask(CAST_FLAGS)),
            p("trigger flags", "Raw TriggerCastFlags; 0 for none", N),
            p("targets at most", "On so many of the targets at most; 0 for all", N),
        ],
    ),
    s(
        75,
        "Add an aura",
        "The target gets an aura (SMART_ACTION_ADD_AURA)",
        &[p("spell", "The aura", Kind::Spell)],
    ),
    s(
        28,
        "Remove an aura",
        "The target loses an aura (SMART_ACTION_REMOVEAURASFROMSPELL)",
        &[
            p("spell", "The aura; none for all", Kind::Spell),
            p("charges", "Charges taken; 0 removes it", N),
        ],
    ),
    s(
        92,
        "Interrupt its cast",
        "The target stops casting (SMART_ACTION_INTERRUPT_SPELL)",
        &[],
    ),
    s(
        1,
        "Say a text",
        "It says a text of its own: says, yells, whispers… (SMART_ACTION_TALK)",
        &[
            p("text", "The text said", Kind::Text),
            p("then wait", "Milliseconds before the event A text is over", MS),
            p(
                "the target the invoker",
                "The first target counts as the one who caused it",
                Kind::Bool,
            ),
            p("to the target", "Said to the target (whispers)", Kind::Bool),
        ],
    ),
    s(
        84,
        "Make the target say a text",
        "Each target says the text (SMART_ACTION_SIMPLE_TALK)",
        &[p("text", "The text said", Kind::Text)],
    ),
    s(
        5,
        "Play an emote",
        "The target plays an emote once (SMART_ACTION_PLAY_EMOTE)",
        &[p("emote", "The emote", Kind::Emote)],
    ),
    s(
        10,
        "Play one of these emotes",
        "The target plays one of the emotes, by chance (SMART_ACTION_RANDOM_EMOTE)",
        &[
            p("emote", "An emote", Kind::Emote),
            p("or", "An emote", Kind::Emote),
            p("or", "An emote", Kind::Emote),
            p("or", "An emote", Kind::Emote),
            p("or", "An emote", Kind::Emote),
            p("or", "An emote", Kind::Emote),
        ],
    ),
    s(
        17,
        "Hold an emote",
        "The target plays an emote until told otherwise (SMART_ACTION_SET_EMOTE_STATE)",
        &[p("emote", "The emote; none to stop", Kind::Emote)],
    ),
    s(
        90,
        "Sit, sleep, kneel…",
        "The target takes a posture (SMART_ACTION_SET_UNIT_FIELD_BYTES_1)",
        &[
            p("posture", "The posture", Kind::Choice(STANDS)),
            p("byte", "0, its posture", N),
        ],
    ),
    s(
        91,
        "Stand up",
        "The target leaves its posture (SMART_ACTION_REMOVE_UNIT_FIELD_BYTES_1)",
        &[
            p("posture", "The posture it leaves", Kind::Choice(STANDS)),
            p("byte", "0, its posture", N),
        ],
    ),
    s(
        40,
        "Draw or sheathe",
        "Its weapons drawn or put away (SMART_ACTION_SET_SHEATH)",
        &[p("weapons", "Which", Kind::Choice(SHEATHS))],
    ),
    s(
        4,
        "Play a sound",
        "A sound (SMART_ACTION_SOUND)",
        &[
            p("sound", "Its id in SoundEntries.dbc", N),
            p("to the target alone", "Only the target hears it", Kind::Bool),
        ],
    ),
    s(
        12,
        "Summon a creature",
        "A creature appears at the target (SMART_ACTION_SUMMON_CREATURE)",
        &[
            p("creature", "The creature", Kind::Creature),
            p("despawns", "When it despawns", Kind::Choice(SUMMON_TYPES)),
            p("for", "Milliseconds", MS),
            p(
                "attacks the invoker",
                "It attacks the one who caused the event",
                Kind::Bool,
            ),
            p("attacks with it", "It attacks the target of the summoner", Kind::Bool),
        ],
    ),
    s(
        107,
        "Summon a group",
        "Creatures of a group of creature_summon_groups appear (SMART_ACTION_SUMMON_CREATURE_GROUP)",
        &[
            p("group", "The group", N),
            p(
                "attack the invoker",
                "They attack the one who caused the event",
                Kind::Bool,
            ),
        ],
    ),
    s(
        50,
        "Summon a game object",
        "A game object appears at the target (SMART_ACTION_SUMMON_GO)",
        &[
            p("game object", "The game object", Kind::GameObject),
            p("for", "Seconds before it despawns", Kind::Number("s")),
        ],
    ),
    s(
        39,
        "Call for help",
        "The creatures near come to help (SMART_ACTION_CALL_FOR_HELP)",
        &[
            p("within", "How far", YD),
            p("with its emote", "Says that it calls for help", Kind::Bool),
        ],
    ),
    s(
        25,
        "Flee for help",
        "It runs to find help (SMART_ACTION_FLEE_FOR_ASSIST)",
        &[p("with its emote", "Says that it flees", Kind::Bool)],
    ),
    s(
        122,
        "Flee",
        "It runs away, feared (SMART_ACTION_FLEE)",
        &[p("for", "Milliseconds", MS)],
    ),
    s(
        24,
        "Evade",
        "It leaves the fight and goes home (SMART_ACTION_EVADE)",
        &[],
    ),
    s(
        27,
        "Stop fighting",
        "It stops the fight (SMART_ACTION_COMBAT_STOP)",
        &[],
    ),
    s(49, "Attack", "It attacks the target (SMART_ACTION_ATTACK_START)", &[]),
    s(
        224,
        "Stop attacking",
        "It stops attacking (SMART_ACTION_ATTACK_STOP)",
        &[],
    ),
    s(
        38,
        "Pull the zone",
        "Every player of the instance enters the fight (SMART_ACTION_SET_IN_COMBAT_WITH_ZONE)",
        &[p("within", "Out of an instance, how far", YD)],
    ),
    s(
        20,
        "Auto-attack",
        "Its melee attacks on or off (SMART_ACTION_AUTO_ATTACK)",
        &[p("on", "Attacks", Kind::Bool)],
    ),
    s(
        21,
        "Move in combat",
        "It moves in combat, or stands where it is (SMART_ACTION_ALLOW_COMBAT_MOVEMENT)",
        &[p("on", "Moves", Kind::Bool)],
    ),
    s(
        79,
        "Keep at a distance",
        "It fights from a distance (SMART_ACTION_SET_RANGED_MOVEMENT)",
        &[
            p("distance", "From its target", YD),
            p("angle", "Around its target, degrees", N),
        ],
    ),
    s(
        8,
        "Reaction",
        "Passive, defensive or aggressive (SMART_ACTION_SET_REACT_STATE)",
        &[p("as", "How it reacts", Kind::Choice(REACT))],
    ),
    s(
        13,
        "Change the target's threat",
        "Its threat on the target, by a percentage (SMART_ACTION_THREAT_SINGLE_PCT)",
        &[p("add", "Percent added", PCT), p("remove", "Percent removed", PCT)],
    ),
    s(
        14,
        "Change all threat",
        "Its threat on everyone, by a percentage (SMART_ACTION_THREAT_ALL_PCT)",
        &[p("add", "Percent added", PCT), p("remove", "Percent removed", PCT)],
    ),
    s(
        22,
        "Set the phase",
        "Its script enters a phase (SMART_ACTION_SET_EVENT_PHASE)",
        &[p("phase", "The phase; 0 for none", Kind::Phase)],
    ),
    s(
        23,
        "Next phase",
        "Its script goes so many phases on (SMART_ACTION_INC_EVENT_PHASE)",
        &[p("by", "Phases on", N), p("back", "Phases back", N)],
    ),
    s(
        30,
        "One of these phases",
        "Its script enters one of the phases, by chance (SMART_ACTION_RANDOM_PHASE)",
        &[
            p("phase", "A phase", Kind::Phase),
            p("or", "A phase", Kind::Phase),
            p("or", "A phase", Kind::Phase),
            p("or", "A phase", Kind::Phase),
            p("or", "A phase", Kind::Phase),
            p("or", "A phase", Kind::Phase),
        ],
    ),
    s(
        31,
        "A phase between",
        "Its script enters a phase between two, by chance (SMART_ACTION_RANDOM_PHASE_RANGE)",
        &[p("from", "The first", Kind::Phase), p("to", "The last", Kind::Phase)],
    ),
    s(
        67,
        "Create a timed event",
        "A timed event of its own, later (SMART_ACTION_CREATE_TIMED_EVENT)",
        &[
            p("event", "Its id, for the event Timed event", N),
            INITIAL_MIN,
            INITIAL_MAX,
            REPEAT_MIN,
            REPEAT_MAX,
            p("chance", "In percent", PCT),
        ],
    ),
    s(
        73,
        "Trigger a timed event",
        "A timed event of its own now (SMART_ACTION_TRIGGER_TIMED_EVENT)",
        &[p("event", "Its id", N)],
    ),
    s(
        74,
        "Remove a timed event",
        "A timed event of its own cancelled (SMART_ACTION_REMOVE_TIMED_EVENT)",
        &[p("event", "Its id", N)],
    ),
    s(
        2,
        "Change faction",
        "The target changes faction (SMART_ACTION_SET_FACTION)",
        &[p("faction", "The faction; none for its own", Kind::Faction)],
    ),
    s(
        3,
        "Change its look",
        "The target looks like another creature or a model (SMART_ACTION_MORPH_TO_ENTRY_OR_MODEL)",
        &[
            p("creature", "Its look; none for a model", Kind::Creature),
            p("model", "A display; none for its own look", Kind::Display),
        ],
    ),
    s(
        43,
        "Mount",
        "The target rides a creature or a model (SMART_ACTION_MOUNT_TO_ENTRY_OR_MODEL)",
        &[
            p("creature", "The mount; none for a model", Kind::Creature),
            p("model", "A display; none to dismount", Kind::Display),
        ],
    ),
    s(
        206,
        "Dismount",
        "The target gets off its mount (SMART_ACTION_DISMOUNT)",
        &[],
    ),
    s(
        227,
        "Scale",
        "The target's size (SMART_ACTION_SET_SCALE)",
        &[p("scale", "In percent of its size", PCT)],
    ),
    s(
        47,
        "Visible",
        "The target seen or not (SMART_ACTION_SET_VISIBILITY)",
        &[p("seen", "Seen", Kind::Bool)],
    ),
    s(
        37,
        "Die",
        "The target dies (SMART_ACTION_DIE)",
        &[p("after", "Milliseconds", MS)],
    ),
    s(51, "Kill", "The target dies (SMART_ACTION_KILL_UNIT)", &[]),
    s(
        41,
        "Despawn",
        "The target disappears (SMART_ACTION_FORCE_DESPAWN)",
        &[
            p("after", "Milliseconds", MS),
            p(
                "respawn after",
                "Seconds before it comes back; 0 as usual",
                Kind::Number("s"),
            ),
        ],
    ),
    s(
        70,
        "Respawn",
        "The target comes back (SMART_ACTION_RESPAWN_TARGET)",
        &[],
    ),
    s(
        42,
        "Cannot die under",
        "Its health never goes under this (SMART_ACTION_SET_INVINCIBILITY_HP_LEVEL)",
        &[
            p("health", "Points; 0 to undo", N),
            p("percent", "In percent instead", PCT),
        ],
    ),
    s(
        142,
        "Set its health",
        "The target's health set (SMART_ACTION_SET_HEALTH_PCT)",
        &[p("health", "In percent", PCT)],
    ),
    s(
        102,
        "Health regeneration",
        "Its health comes back or not (SMART_ACTION_SET_HEALTH_REGEN)",
        &[p("on", "Comes back", Kind::Bool)],
    ),
    s(
        108,
        "Set its power",
        "The target's power (SMART_ACTION_SET_POWER)",
        &[p("power", "Which", Kind::Choice(POWERS)), p("to", "Points", N)],
    ),
    s(
        117,
        "No evading",
        "It never evades (SMART_ACTION_DISABLE_EVADE)",
        &[p("on", "Never evades", Kind::Bool)],
    ),
    s(
        103,
        "Rooted",
        "It cannot move (SMART_ACTION_SET_ROOT)",
        &[p("on", "Rooted", Kind::Bool)],
    ),
    s(
        59,
        "Run",
        "It runs or walks (SMART_ACTION_SET_RUN)",
        &[p("on", "Runs", Kind::Bool)],
    ),
    s(
        207,
        "Hover",
        "It hovers (SMART_ACTION_SET_HOVER)",
        &[p("on", "Hovers", Kind::Bool)],
    ),
    s(
        136,
        "Speed",
        "Its speed (SMART_ACTION_SET_MOVEMENT_SPEED)",
        &[
            p(
                "of",
                "Which",
                Kind::Choice(&[(0, "walk"), (1, "run"), (3, "swim"), (5, "flight")]),
            ),
            p("whole", "The whole part", N),
            p("hundredths", "The part after the point, in hundredths", N),
        ],
    ),
    s(
        89,
        "Wander",
        "It wanders about (SMART_ACTION_RANDOM_MOVE)",
        &[p("within", "How far; 0 to stop", YD)],
    ),
    s(
        69,
        "Move to",
        "It walks to the target (SMART_ACTION_MOVE_TO_POS)",
        &[
            p("point", "Its id, for the event Movement ends", N),
            p("on a transport", "On a transport", Kind::Bool),
        ],
    ),
    s(
        201,
        "Move to its target",
        "It walks to its target (SMART_ACTION_MOVE_TO_POS_TARGET)",
        &[p("point", "Its id, for the event Movement ends", N)],
    ),
    s(
        46,
        "Move forward",
        "It walks ahead (SMART_ACTION_MOVE_FORWARD)",
        &[p("by", "How far", YD)],
    ),
    s(
        97,
        "Jump to",
        "It jumps to the target (SMART_ACTION_JUMP_TO_POS)",
        &[
            p("speed across", "Its speed along the ground", N),
            p("speed up", "Its speed upwards", N),
        ],
    ),
    s(
        29,
        "Follow",
        "It follows the target (SMART_ACTION_FOLLOW)",
        &[
            p("distance", "Behind it; 0 as usual", YD),
            p("angle", "Degrees; 0 as usual", N),
        ],
    ),
    s(66, "Face", "It turns to the target (SMART_ACTION_SET_ORIENTATION)", &[]),
    s(
        101,
        "Make here its home",
        "Its home is where it is (SMART_ACTION_SET_HOME_POS)",
        &[],
    ),
    s(
        232,
        "Walk a path",
        "It walks a path of waypoints (SMART_ACTION_WAYPOINT_START)",
        &[
            p("path", "The path (waypoint_data.id)", N),
            p("again and again", "It starts again at its end", Kind::Bool),
        ],
    ),
    s(234, "Stop walking", "It stops (SMART_ACTION_MOVEMENT_STOP)", &[]),
    s(
        235,
        "Pause",
        "It stops for a while (SMART_ACTION_MOVEMENT_PAUSE)",
        &[p("for", "Milliseconds", MS)],
    ),
    s(236, "Go on", "It walks again (SMART_ACTION_MOVEMENT_RESUME)", &[]),
    s(
        81,
        "Set its roles",
        "Its npcflag (SMART_ACTION_SET_NPC_FLAG)",
        &[p("flags", "npcflag", N)],
    ),
    s(
        82,
        "Add roles",
        "Flags added to its npcflag (SMART_ACTION_ADD_NPC_FLAG)",
        &[p("flags", "npcflag", N)],
    ),
    s(
        83,
        "Remove roles",
        "Flags taken from its npcflag (SMART_ACTION_REMOVE_NPC_FLAG)",
        &[p("flags", "npcflag", N)],
    ),
    s(
        18,
        "Set unit flags",
        "Flags added to its unit_flags (SMART_ACTION_SET_UNIT_FLAG)",
        &[p("flags", "unit_flags", N)],
    ),
    s(
        19,
        "Remove unit flags",
        "Flags taken from its unit_flags (SMART_ACTION_REMOVE_UNIT_FLAG)",
        &[p("flags", "unit_flags", N)],
    ),
    s(
        71,
        "Equip",
        "It holds other weapons (SMART_ACTION_EQUIP)",
        &[
            p("equipment", "creature_equip_template.ID; 0 for the items after", N),
            p("slots", "Raw mask: 1 main, 2 off, 4 ranged; 0 all", N),
            p("main hand", "An item", Kind::Item),
            p("off hand", "An item", Kind::Item),
            p("ranged", "An item", Kind::Item),
        ],
    ),
    s(
        7,
        "Offer a quest",
        "The target is offered a quest (SMART_ACTION_OFFER_QUEST)",
        &[
            p("quest", "The quest", Kind::Quest),
            p("added at once", "Into its log without asking", Kind::Bool),
        ],
    ),
    s(
        6,
        "Fail a quest",
        "The target's quest fails (SMART_ACTION_FAIL_QUEST)",
        &[p("quest", "The quest", Kind::Quest)],
    ),
    s(
        15,
        "Explore for a quest",
        "The target's quest of exploration is done (SMART_ACTION_CALL_AREAEXPLOREDOREVENTHAPPENS)",
        &[p("quest", "The quest", Kind::Quest)],
    ),
    s(
        33,
        "Count a kill",
        "The target is credited with a kill of a creature (SMART_ACTION_CALL_KILLEDMONSTER)",
        &[p("creature", "The creature counted", Kind::Creature)],
    ),
    s(
        56,
        "Give an item",
        "The target gets items (SMART_ACTION_ADD_ITEM)",
        &[p("item", "The item", Kind::Item), p("count", "How many", N)],
    ),
    s(
        57,
        "Take an item",
        "The target loses items (SMART_ACTION_REMOVE_ITEM)",
        &[p("item", "The item", Kind::Item), p("count", "How many", N)],
    ),
    s(
        45,
        "Set a datum",
        "A datum set on the target, its event Data set (SMART_ACTION_SET_DATA)",
        &[p("field", "The field", N), p("value", "The value", N)],
    ),
    s(
        63,
        "Set a counter",
        "A counter of its own (SMART_ACTION_SET_COUNTER)",
        &[
            p("counter", "The counter", N),
            p("value", "Added", N),
            p("reset", "Set to it instead", Kind::Bool),
        ],
    ),
    s(
        64,
        "Remember the targets",
        "The targets kept, for the target Stored (SMART_ACTION_STORE_TARGET_LIST)",
        &[p("as", "The list's id", N)],
    ),
    s(
        80,
        "Run a list of actions",
        "A timed list of actions (smart_scripts of source 9) (SMART_ACTION_CALL_TIMED_ACTIONLIST)",
        &[
            p("list", "The list's id", N),
            p("stops after combat", "Stops when the fight ends", Kind::Bool),
            p(
                "runs",
                "When its timers run",
                Kind::Choice(&[(0, "out of combat"), (1, "in combat"), (2, "always")]),
            ),
        ],
    ),
    s(
        78,
        "Reset its script",
        "Its script starts again (SMART_ACTION_CALL_SCRIPT_RESET)",
        &[],
    ),
    s(
        62,
        "Teleport",
        "The target is teleported, to the target's position (SMART_ACTION_TELEPORT)",
        &[p("map", "The map", N)],
    ),
    s(
        229,
        "Play a spell visual",
        "A visual of a spell, without its effects (SMART_ACTION_PLAY_SPELL_VISUAL)",
        &[
            p("visual", "SpellVisual.dbc", N),
            p("on impact", "SpellVisual.dbc of its impact", N),
        ],
    ),
    s(
        216,
        "Play music",
        "A music (SMART_ACTION_MUSIC)",
        &[
            p("sound", "SoundEntries.dbc", N),
            p("to the target alone", "Only the target hears it", Kind::Bool),
        ],
    ),
    s(
        239,
        "Ground or air",
        "It stands, hovers or flies (SMART_ACTION_SET_ANIM_TIER)",
        &[p(
            "tier",
            "Its tier",
            Kind::Choice(&[
                (0, "ground"),
                (1, "swimming"),
                (2, "hovering"),
                (3, "flying"),
                (4, "submerged"),
            ]),
        )],
    ),
    s(
        240,
        "Set its gossip",
        "Its gossip menu changed (SMART_ACTION_SET_GOSSIP_MENU)",
        &[p("menu", "gossip_menu.MenuID", N)],
    ),
    s(
        72,
        "Close the gossip",
        "The target's gossip window closes (SMART_ACTION_CLOSE_GOSSIP)",
        &[],
    ),
    s(
        121,
        "Sight",
        "How far it sees (SMART_ACTION_SET_SIGHT_DIST)",
        &[p("distance", "How far", YD)],
    ),
    s(
        205,
        "Combat distance",
        "How far it fights from (SMART_ACTION_SET_COMBAT_DISTANCE)",
        &[p("distance", "How far", YD)],
    ),
    s(
        223,
        "Do an action",
        "An action of its C++ script (SMART_ACTION_DO_ACTION)",
        &[p("action", "The action's id", N)],
    ),
];

const ALIVE: &[(i64, &str)] = &[(0, "alive or dead"), (1, "alive"), (2, "dead")];

/// The targets (`SMARTAI_TARGETS`).
pub const TARGETS: &[Spec] = &[
    s(1, "Itself", "The NPC itself (SMART_TARGET_SELF)", &[]),
    s(
        2,
        "Its target",
        "The one it fights, its highest threat (SMART_TARGET_VICTIM)",
        &[],
    ),
    s(
        7,
        "The invoker",
        "The one who caused the event (SMART_TARGET_ACTION_INVOKER)",
        &[],
    ),
    s(
        16,
        "The invoker's party",
        "The invoker and its party (SMART_TARGET_INVOKER_PARTY)",
        &[p("with pets", "Their pets too", Kind::Bool)],
    ),
    s(
        5,
        "Someone it fights, at random",
        "One of its threat list by chance (SMART_TARGET_HOSTILE_RANDOM)",
        &[
            p("within", "How far; 0 any", YD),
            p("players alone", "Only players", Kind::Bool),
        ],
    ),
    s(
        6,
        "Someone it fights but its target",
        "One of its threat list but the first, by chance (SMART_TARGET_HOSTILE_RANDOM_NOT_TOP)",
        &[
            p("within", "How far; 0 any", YD),
            p("players alone", "Only players", Kind::Bool),
        ],
    ),
    s(
        3,
        "Its second on threat",
        "The second of its threat list (SMART_TARGET_HOSTILE_SECOND_AGGRO)",
        &[
            p("within", "How far; 0 any", YD),
            p("players alone", "Only players", Kind::Bool),
        ],
    ),
    s(
        4,
        "Its last on threat",
        "The last of its threat list (SMART_TARGET_HOSTILE_LAST_AGGRO)",
        &[
            p("within", "How far; 0 any", YD),
            p("players alone", "Only players", Kind::Bool),
        ],
    ),
    s(
        24,
        "Everyone it fights",
        "Its whole threat list (SMART_TARGET_THREAT_LIST)",
        &[
            p("within", "How far; 0 any", YD),
            p("players alone", "Only players", Kind::Bool),
        ],
    ),
    s(
        28,
        "The farthest it fights",
        "The farthest of its threat list (SMART_TARGET_FARTHEST)",
        &[
            p("within", "How far", YD),
            p("players alone", "Only players", Kind::Bool),
            p("in sight", "Only those it sees", Kind::Bool),
        ],
    ),
    s(
        25,
        "The closest enemy",
        "The nearest enemy (SMART_TARGET_CLOSEST_ENEMY)",
        &[
            p("within", "How far", YD),
            p("players alone", "Only players", Kind::Bool),
        ],
    ),
    s(
        26,
        "The closest ally",
        "The nearest ally (SMART_TARGET_CLOSEST_FRIENDLY)",
        &[
            p("within", "How far", YD),
            p("players alone", "Only players", Kind::Bool),
        ],
    ),
    s(
        21,
        "The closest player",
        "The nearest player (SMART_TARGET_CLOSEST_PLAYER)",
        &[p("within", "How far", YD)],
    ),
    s(
        18,
        "The players near",
        "Every player within a distance (SMART_TARGET_PLAYER_DISTANCE)",
        &[p("within", "How far", YD)],
    ),
    s(
        17,
        "The players between",
        "The players between two distances (SMART_TARGET_PLAYER_RANGE)",
        &[
            p("from", "The nearest", YD),
            p("to", "The farthest", YD),
            p("at most", "How many; 0 all", N),
        ],
    ),
    s(
        201,
        "The players with an aura",
        "The players near who have an aura (SMART_TARGET_PLAYER_WITH_AURA)",
        &[
            p("spell", "The aura", Kind::Spell),
            p("without it", "Those who have it NOT", Kind::Bool),
            p("within", "How far", YD),
        ],
    ),
    s(
        19,
        "The closest creature",
        "The nearest creature of an entry (SMART_TARGET_CLOSEST_CREATURE)",
        &[
            p("creature", "None for any", Kind::Creature),
            p("within", "How far", YD),
            p("dead", "A dead one", Kind::Bool),
        ],
    ),
    s(
        11,
        "The creatures near",
        "The creatures of an entry within a distance (SMART_TARGET_CREATURE_DISTANCE)",
        &[
            p("creature", "None for any", Kind::Creature),
            p("within", "How far", YD),
            p("which", "Alive or dead", Kind::Choice(ALIVE)),
        ],
    ),
    s(
        9,
        "The creatures between",
        "The creatures of an entry between two distances (SMART_TARGET_CREATURE_RANGE)",
        &[
            p("creature", "None for any", Kind::Creature),
            p("from", "The nearest", YD),
            p("to", "The farthest", YD),
            p("which", "Alive or dead", Kind::Choice(ALIVE)),
        ],
    ),
    s(
        204,
        "Its summons",
        "The creatures it summoned (SMART_TARGET_SUMMONED_CREATURES)",
        &[p("creature", "None for any", Kind::Creature)],
    ),
    s(
        23,
        "Its owner or summoner",
        "Who owns or summoned it (SMART_TARGET_OWNER_OR_SUMMONER)",
        &[],
    ),
    s(
        206,
        "Its group",
        "The members of its formation (SMART_TARGET_FORMATION)",
        &[
            p(
                "who",
                "Which",
                Kind::Choice(&[(0, "members"), (1, "leader"), (2, "all")]),
            ),
            p("creature", "None for any", Kind::Creature),
            p("not itself", "Itself left out", Kind::Bool),
        ],
    ),
    s(
        20,
        "The closest game object",
        "The nearest game object of an entry (SMART_TARGET_CLOSEST_GAMEOBJECT)",
        &[
            p("game object", "None for any", Kind::GameObject),
            p("within", "How far", YD),
        ],
    ),
    s(
        15,
        "The game objects near",
        "The game objects of an entry within a distance (SMART_TARGET_GAMEOBJECT_DISTANCE)",
        &[
            p("game object", "None for any", Kind::GameObject),
            p("within", "How far", YD),
        ],
    ),
    s(
        12,
        "The targets remembered",
        "Those kept by Remember the targets (SMART_TARGET_STORED)",
        &[p("list", "The list's id", N)],
    ),
    s(
        27,
        "Those who tagged it",
        "The players who earn its kill (SMART_TARGET_LOOT_RECIPIENTS)",
        &[],
    ),
    s(
        8,
        "A position",
        "A place, its coordinates below (SMART_TARGET_POSITION)",
        &[],
    ),
    s(
        202,
        "A point at random",
        "A point near by chance (SMART_TARGET_RANDOM_POINT)",
        &[
            p("within", "How far", YD),
            p("how many", "Points", N),
            p("around itself", "Around itself, else the coordinates", Kind::Bool),
        ],
    ),
    s(0, "None", "No target (SMART_TARGET_NONE)", &[]),
];

/// The flags of an event (`SmartEventFlags`).
pub const EVENT_FLAGS: &[(i64, &str, &str)] = &[
    (
        0x1,
        "Once",
        "It happens once, until the NPC respawns (SMART_EVENT_FLAG_NOT_REPEATABLE)",
    ),
    (
        0x2,
        "Normal (10)",
        "Only in this difficulty (SMART_EVENT_FLAG_DIFFICULTY_0)",
    ),
    (
        0x4,
        "Heroic (25)",
        "Only in this difficulty (SMART_EVENT_FLAG_DIFFICULTY_1)",
    ),
    (
        0x8,
        "10 heroic",
        "Only in this difficulty (SMART_EVENT_FLAG_DIFFICULTY_2)",
    ),
    (
        0x10,
        "25 heroic",
        "Only in this difficulty (SMART_EVENT_FLAG_DIFFICULTY_3)",
    ),
    (
        0x100,
        "Not reset",
        "It keeps its timers after a fight (SMART_EVENT_FLAG_DONT_RESET)",
    ),
    (
        0x200,
        "Even charmed",
        "It happens while a player charms it (SMART_EVENT_FLAG_WHILE_CHARMED)",
    ),
];

pub fn event(id: i64) -> Option<&'static Spec> {
    EVENTS.iter().find(|spec| spec.id == id)
}

pub fn action(id: i64) -> Option<&'static Spec> {
    ACTIONS.iter().find(|spec| spec.id == id)
}

pub fn target(id: i64) -> Option<&'static Spec> {
    TARGETS.iter().find(|spec| spec.id == id)
}

/// A skill as its prefab keeps it: an event, an action, a target, each its kind and parameters;
/// the phases it happens in, its chance, its flags, whether the next skill follows it at once
/// (`link`), what it is said to be.
#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub event: i64,
    pub event_params: [i64; 6],
    pub phases: i64,
    pub chance: i64,
    pub flags: i64,
    pub action: i64,
    pub action_params: [i64; 6],
    pub target: i64,
    pub target_params: [i64; 4],
    pub position: [f32; 4],
    pub linked: bool,
    pub comment: String,
}

impl Default for Skill {
    fn default() -> Self {
        Self {
            event: 0,
            event_params: [3000, 6000, 8000, 12000, 0, 0],
            phases: 0,
            chance: 100,
            flags: 0,
            action: 11,
            action_params: [0; 6],
            target: 2,
            target_params: [0; 4],
            position: [0.0; 4],
            linked: false,
            comment: String::new(),
        }
    }
}

fn numbers<const L: usize>(value: &Value) -> [i64; L] {
    let mut out = [0; L];
    for (at, slot) in out.iter_mut().enumerate() {
        *slot = value.get(at).and_then(Value::as_i64).unwrap_or(0);
    }
    out
}

impl Skill {
    pub fn to_json(&self) -> Value {
        json!({
            "event": { "type": self.event, "params": self.event_params, "phases": self.phases, "chance": self.chance, "flags": self.flags },
            "action": { "type": self.action, "params": self.action_params },
            "target": { "type": self.target, "params": self.target_params, "position": self.position },
            "linked": self.linked,
            "comment": self.comment,
        })
    }

    pub fn of_json(value: &Value) -> Self {
        let position = value["target"]["position"]
            .as_array()
            .map(|at| {
                let mut out = [0.0f32; 4];
                for (k, slot) in out.iter_mut().enumerate() {
                    *slot = at.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
                }
                out
            })
            .unwrap_or_default();
        Self {
            event: value["event"]["type"].as_i64().unwrap_or(0),
            event_params: numbers(&value["event"]["params"]),
            phases: value["event"]["phases"].as_i64().unwrap_or(0),
            chance: value["event"]["chance"].as_i64().unwrap_or(100),
            flags: value["event"]["flags"].as_i64().unwrap_or(0),
            action: value["action"]["type"].as_i64().unwrap_or(0),
            action_params: numbers(&value["action"]["params"]),
            target: value["target"]["type"].as_i64().unwrap_or(0),
            target_params: numbers(&value["target"]["params"]),
            position,
            linked: value["linked"].as_bool().unwrap_or(false),
            comment: value["comment"].as_str().unwrap_or_default().to_owned(),
        }
    }
}

/// Names what a parameter's value names: a spell, a creature… by id; none when not known.
pub trait Names {
    fn named(&self, kind: Kind, value: i64) -> Option<String>;
}

/// A value of a parameter said: named where it names something, its unit after a number.
pub fn said(param: &Param, value: i64, names: &dyn Names) -> String {
    match param.kind {
        Kind::Number(unit) if unit == "ms" && value >= 1000 && value % 100 == 0 => {
            format!("{} s", value as f64 / 1000.0)
        }
        Kind::Number("") => value.to_string(),
        Kind::Number(unit) => format!("{value} {unit}"),
        Kind::Bool => (if value != 0 { "yes" } else { "no" }).to_owned(),
        Kind::Choice(values) => values
            .iter()
            .find(|(held, _)| *held == value)
            .map_or(value.to_string(), |(_, name)| (*name).to_owned()),
        Kind::Mask(bits) => {
            let named: Vec<&str> = bits
                .iter()
                .filter(|(bit, _)| value & bit != 0)
                .map(|(_, name)| *name)
                .collect();
            if named.is_empty() {
                "none".to_owned()
            } else {
                named.join(", ")
            }
        }
        Kind::Phase if value == 0 => "none".to_owned(),
        Kind::Phase => format!("phase {value}"),
        Kind::Phases => phases_said(value),
        kind if value == 0 => match kind {
            Kind::Spell | Kind::Creature | Kind::GameObject | Kind::Item | Kind::Quest => "any".to_owned(),
            _ => "none".to_owned(),
        },
        kind => names.named(kind, value).unwrap_or_else(|| value.to_string()),
    }
}

/// A mask of phases said: "every phase" for none.
pub fn phases_said(mask: i64) -> String {
    if mask == 0 {
        return "every phase".to_owned();
    }
    let phases: Vec<String> = (1..=12)
        .filter(|k| mask & (1 << (k - 1)) != 0)
        .map(|k| k.to_string())
        .collect();
    format!("phase {}", phases.join(", "))
}

/// A skill said in a line: its event, its action, its target, the parameters that are set.
pub fn sentence(skill: &Skill, names: &dyn Names) -> String {
    let part = |spec: Option<&Spec>, kind: i64, values: &[i64]| -> String {
        let Some(spec) = spec else {
            return format!("#{kind}");
        };
        let set: Vec<String> = spec
            .params
            .iter()
            .zip(values)
            .filter(|(param, value)| **value != 0 || matches!(param.kind, Kind::Number("%")))
            .filter(|(param, _)| !matches!(param.kind, Kind::Bool) || values.iter().any(|v| *v != 0))
            .map(|(param, value)| format!("{} {}", param.name, said(param, *value, names)))
            .collect();
        if set.is_empty() {
            spec.name.to_lowercase()
        } else {
            format!("{} ({})", spec.name.to_lowercase(), set.join(", "))
        }
    };
    let mut out = format!(
        "{} → {} → {}",
        part(event(skill.event), skill.event, &skill.event_params),
        part(action(skill.action), skill.action, &skill.action_params),
        part(target(skill.target), skill.target, &skill.target_params)
    );
    if skill.phases != 0 {
        out.push_str(&format!(" [{}]", phases_said(skill.phases)));
    }
    if skill.chance < 100 {
        out.push_str(&format!(" [{} %]", skill.chance));
    }
    out
}

/// Skills ready made: what a designer asks most, in words.
#[allow(clippy::vec_init_then_push)]
pub fn presets() -> Vec<(&'static str, &'static str, Skill)> {
    let mut out = Vec::new();
    out.push((
        "Casts a spell in combat",
        "Casts a spell on its target every 8 to 12 s while it fights",
        Skill::default(),
    ));
    out.push((
        "Heals itself when hurt",
        "Casts a spell on itself once, under 30 % of its health",
        Skill {
            event: 2,
            event_params: [0, 30, 0, 0, 0, 0],
            flags: 0x1,
            target: 1,
            ..Skill::default()
        },
    ));
    out.push((
        "Calls for help on aggro",
        "Calls the creatures within 20 yards when it starts to fight",
        Skill {
            event: 4,
            event_params: [0; 6],
            action: 39,
            action_params: [20, 1, 0, 0, 0, 0],
            target: 1,
            ..Skill::default()
        },
    ));
    out.push((
        "Flees when hurt",
        "Flees for help once, under 15 % of its health",
        Skill {
            event: 2,
            event_params: [0, 15, 0, 0, 0, 0],
            flags: 0x1,
            action: 25,
            action_params: [1, 0, 0, 0, 0, 0],
            target: 1,
            ..Skill::default()
        },
    ));
    out.push((
        "Says a text on aggro",
        "Says a text of its own when it starts to fight",
        Skill {
            event: 4,
            event_params: [0; 6],
            action: 1,
            target: 2,
            ..Skill::default()
        },
    ));
    out.push((
        "Says a text on death",
        "Says a text of its own when it dies",
        Skill {
            event: 6,
            event_params: [0; 6],
            action: 1,
            target: 7,
            ..Skill::default()
        },
    ));
    out.push((
        "Says a text on a kill",
        "Says a text of its own when it kills a player",
        Skill {
            event: 5,
            event_params: [5000, 5000, 1, 0, 0, 0],
            action: 1,
            target: 7,
            ..Skill::default()
        },
    ));
    out.push((
        "Plays an emote out of combat",
        "Plays an emote every 20 to 40 s while it does not fight",
        Skill {
            event: 1,
            event_params: [5000, 15000, 20000, 40000, 0, 0],
            action: 5,
            target: 1,
            ..Skill::default()
        },
    ));
    out.push((
        "Enrages under 20 %",
        "Casts a spell on itself once under 20 % of its health",
        Skill {
            event: 2,
            event_params: [0, 20, 0, 0, 0, 0],
            flags: 0x1,
            target: 1,
            ..Skill::default()
        },
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoNames;
    impl Names for NoNames {
        fn named(&self, _: Kind, value: i64) -> Option<String> {
            (value == 133).then(|| "Fireball".to_owned())
        }
    }

    #[test]
    fn every_kind_is_once_its_parameters_six_at_most_and_said() {
        for (list, most) in [(EVENTS, 6), (ACTIONS, 6), (TARGETS, 4)] {
            let mut ids: Vec<i64> = list.iter().map(|spec| spec.id).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), list.len());
            for spec in list {
                assert!(spec.params.len() <= most, "{}", spec.name);
                assert!(
                    !spec.hint.is_empty() && spec.params.iter().all(|param| !param.hint.is_empty()),
                    "{}",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn a_skill_is_said_in_words_and_kept_as_json() {
        let skill = Skill {
            action_params: [133, 0, 0, 0, 0, 0],
            ..Skill::default()
        };
        assert_eq!(
            sentence(&skill, &NoNames),
            "in combat, every… (first after, from 3 s, to 6 s, then every, from 8 s, to 12 s) → cast a spell (spell Fireball) → its target"
        );
        assert_eq!(Skill::of_json(&skill.to_json()), skill);
        assert_eq!(phases_said(0b101), "phase 1, 3");
        assert!(presets().len() >= 8);
    }
}
