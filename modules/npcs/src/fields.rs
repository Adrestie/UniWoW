//! What the columns of a creature's template mean, as AzerothCore's UnitDefines.h, CreatureData.h
//! and SharedDefines.h say it: the masks as boxes of named flags, the choices as named lists, the
//! numbers each with what it does in the game; never a number to guess.

/// A flag of a mask: its bit, its name, what it does in the game.
pub struct Flag {
    pub bit: u32,
    pub name: &'static str,
    pub hint: &'static str,
}

const fn flag(bit: u32, name: &'static str, hint: &'static str) -> Flag {
    Flag { bit, name, hint }
}

/// A value of a choice: the value, its name, what it does.
pub type Choice = (i64, &'static str, &'static str);

/// What it offers the players who speak to it (`npcflag`, `NPCFlags`).
pub const NPC_FLAGS: [Flag; 25] = [
    flag(
        0x1,
        "Gossip",
        "Speaks: a menu of gossip when clicked (UNIT_NPC_FLAG_GOSSIP)",
    ),
    flag(
        0x2,
        "Quest giver",
        "Gives or takes quests, its ! and ? above it (UNIT_NPC_FLAG_QUESTGIVER)",
    ),
    flag(0x10, "Trainer", "Teaches spells (UNIT_NPC_FLAG_TRAINER)"),
    flag(
        0x20,
        "Class trainer",
        "Teaches the spells of a class (UNIT_NPC_FLAG_TRAINER_CLASS)",
    ),
    flag(
        0x40,
        "Profession trainer",
        "Teaches a profession (UNIT_NPC_FLAG_TRAINER_PROFESSION)",
    ),
    flag(0x80, "Vendor", "Sells and buys items (UNIT_NPC_FLAG_VENDOR)"),
    flag(
        0x100,
        "Ammunition vendor",
        "Sells ammunition and general goods (UNIT_NPC_FLAG_VENDOR_AMMO)",
    ),
    flag(0x200, "Food vendor", "Sells food and drink (UNIT_NPC_FLAG_VENDOR_FOOD)"),
    flag(0x400, "Poison vendor", "Sells poisons (UNIT_NPC_FLAG_VENDOR_POISON)"),
    flag(0x800, "Reagent vendor", "Sells reagents (UNIT_NPC_FLAG_VENDOR_REAGENT)"),
    flag(
        0x1000,
        "Repairs",
        "Repairs the players' equipment (UNIT_NPC_FLAG_REPAIR)",
    ),
    flag(
        0x2000,
        "Flight master",
        "Flies the players to the places they know (UNIT_NPC_FLAG_FLIGHTMASTER)",
    ),
    flag(
        0x4000,
        "Spirit healer",
        "Brings the dead back to life at a graveyard (UNIT_NPC_FLAG_SPIRITHEALER)",
    ),
    flag(
        0x8000,
        "Spirit guide",
        "The spirit of a battleground's graveyard (UNIT_NPC_FLAG_SPIRITGUIDE)",
    ),
    flag(
        0x10000,
        "Innkeeper",
        "Makes an inn the players' home (UNIT_NPC_FLAG_INNKEEPER)",
    ),
    flag(0x20000, "Banker", "Opens the players' bank (UNIT_NPC_FLAG_BANKER)"),
    flag(
        0x40000,
        "Petitioner",
        "Sells the charters of guilds and arena teams (UNIT_NPC_FLAG_PETITIONER)",
    ),
    flag(
        0x80000,
        "Tabard designer",
        "Designs the tabard of a guild (UNIT_NPC_FLAG_TABARDDESIGNER)",
    ),
    flag(
        0x100000,
        "Battlemaster",
        "Queues the players for a battleground (UNIT_NPC_FLAG_BATTLEMASTER)",
    ),
    flag(
        0x200000,
        "Auctioneer",
        "Opens the auction house (UNIT_NPC_FLAG_AUCTIONEER)",
    ),
    flag(
        0x400000,
        "Stable master",
        "Keeps the hunters' pets (UNIT_NPC_FLAG_STABLEMASTER)",
    ),
    flag(
        0x800000,
        "Guild banker",
        "Opens the guild's bank (UNIT_NPC_FLAG_GUILD_BANKER)",
    ),
    flag(
        0x1000000,
        "Spell click",
        "Casts a spell on the player who clicks it, as a vehicle or a portal (UNIT_NPC_FLAG_SPELLCLICK)",
    ),
    flag(
        0x2000000,
        "Player vehicle",
        "A vehicle the players ride (UNIT_NPC_FLAG_PLAYER_VEHICLE)",
    ),
    flag(0x4000000, "Mailbox", "Opens the players' mail (UNIT_NPC_FLAG_MAILBOX)"),
];

/// How it stands to the players and the world (`unit_flags`, `UnitFlags`); those the server sets
/// itself while it plays (in combat, looting, fleeing…) left out.
pub const UNIT_FLAGS: [Flag; 14] = [
    flag(0x2, "Not attackable", "Nobody can attack it (UNIT_FLAG_NON_ATTACKABLE)"),
    flag(
        0x4,
        "Cannot move",
        "It never moves, even in combat (UNIT_FLAG_DISABLE_MOVE)",
    ),
    flag(
        0x100,
        "Immune to players",
        "No fight with the players: they cannot attack it nor it them (UNIT_FLAG_IMMUNE_TO_PC)",
    ),
    flag(
        0x200,
        "Immune to creatures",
        "No fight with the other creatures (UNIT_FLAG_IMMUNE_TO_NPC)",
    ),
    flag(
        0x1000,
        "PvP",
        "Flagged for PvP: attacking it flags the player (UNIT_FLAG_PVP)",
    ),
    flag(0x2000, "Silenced", "It cannot cast (UNIT_FLAG_SILENCED)"),
    flag(
        0x4000,
        "Cannot swim",
        "It stays out of the water (UNIT_FLAG_CANNOT_SWIM)",
    ),
    flag(0x8000, "Swims", "Drawn swimming in the water (UNIT_FLAG_SWIMMING)"),
    flag(0x20000, "Pacified", "It cannot attack (UNIT_FLAG_PACIFIED)"),
    flag(0x40000, "Stunned", "Drawn stunned, it does nothing (UNIT_FLAG_STUNNED)"),
    flag(
        0x2000000,
        "Not selectable",
        "It cannot be clicked nor targeted (UNIT_FLAG_NOT_SELECTABLE)",
    ),
    flag(
        0x4000000,
        "Skinnable",
        "Its corpse can be skinned (UNIT_FLAG_SKINNABLE)",
    ),
    flag(
        0x20000000,
        "No chat emotes",
        "Its texts never make it play an emote (UNIT_FLAG_PREVENT_EMOTES_FROM_CHAT_TEXT)",
    ),
    flag(0x80000000, "Immune to damage", "Nothing hurts it (UNIT_FLAG_IMMUNE)"),
];

/// More of how it stands (`unit_flags2`, `UnitFlags2`).
pub const UNIT_FLAGS2: [Flag; 11] = [
    flag(0x1, "Feigns death", "Lies as dead (UNIT_FLAG2_FEIGN_DEATH)"),
    flag(
        0x2,
        "Body hidden",
        "Its body is not drawn, its weapons alone (UNIT_FLAG2_HIDE_BODY)",
    ),
    flag(
        0x4,
        "Ignores reputation",
        "Its hostility follows its faction alone, not the players' reputation (UNIT_FLAG2_IGNORE_REPUTATION)",
    ),
    flag(
        0x8,
        "Understands every language",
        "The players understand what it says whatever its language (UNIT_FLAG2_COMPREHEND_LANG)",
    ),
    flag(
        0x20,
        "Appears at once",
        "It does not fade in when it appears (UNIT_FLAG2_DO_NOT_FADE_IN)",
    ),
    flag(
        0x80,
        "Off hand disarmed",
        "Its off hand weapon is not used (UNIT_FLAG2_DISARM_OFFHAND)",
    ),
    flag(
        0x400,
        "Ranged disarmed",
        "Its ranged weapon is not used (UNIT_FLAG2_DISARM_RANGED)",
    ),
    flag(
        0x800,
        "Regenerates power",
        "Its mana or energy comes back (UNIT_FLAG2_REGENERATE_POWER); the usual",
    ),
    flag(
        0x1000,
        "Party alone",
        "Only the players of one party or raid can speak to it (UNIT_FLAG2_RESTRICT_PARTY_INTERACTION)",
    ),
    flag(0x8000, "Cannot turn", "It never turns to face (UNIT_FLAG2_CANNOT_TURN)"),
    flag(
        0x20000,
        "Death animation",
        "It plays a special animation when it dies (UNIT_FLAG2_PLAY_DEATH_ANIM)",
    ),
];

/// What the client knows of its kind (`type_flags`, `CreatureTypeFlags`).
pub const TYPE_FLAGS: [Flag; 16] = [
    flag(
        0x1,
        "Tameable",
        "A hunter can tame it: a beast with a family (CREATURE_TYPE_FLAG_TAMEABLE)",
    ),
    flag(
        0x10000,
        "Tameable (exotic)",
        "Tamed by the hunters who master exotic beasts (CREATURE_TYPE_FLAG_TAMEABLE_EXOTIC)",
    ),
    flag(
        0x2,
        "Seen by ghosts",
        "The dead players see it and speak to it too (CREATURE_TYPE_FLAG_VISIBLE_TO_GHOSTS)",
    ),
    flag(
        0x4,
        "Boss",
        "Its level shown as ?? and immune to knockback (CREATURE_TYPE_FLAG_BOSS_MOB)",
    ),
    flag(
        0x8,
        "No wound animation",
        "It does not flinch when it parries (CREATURE_TYPE_FLAG_DO_NOT_PLAY_WOUND_ANIM)",
    ),
    flag(
        0x10,
        "No faction in tooltip",
        "Its tooltip does not name its faction (CREATURE_TYPE_FLAG_NO_FACTION_TOOLTIP)",
    ),
    flag(
        0x80,
        "Interaction while dead",
        "The players speak to it once it is dead (CREATURE_TYPE_FLAG_INTERACT_WHILE_DEAD)",
    ),
    flag(
        0x100,
        "Herbalism",
        "Its corpse is gathered by herbalism (CREATURE_TYPE_FLAG_SKIN_WITH_HERBALISM)",
    ),
    flag(
        0x200,
        "Mining",
        "Its corpse is gathered by mining (CREATURE_TYPE_FLAG_SKIN_WITH_MINING)",
    ),
    flag(
        0x8000,
        "Engineering",
        "Its corpse is gathered by engineering (CREATURE_TYPE_FLAG_SKIN_WITH_ENGINEERING)",
    ),
    flag(
        0x400,
        "No death message",
        "Its death is not in the combat log (CREATURE_TYPE_FLAG_NO_DEATH_MESSAGE)",
    ),
    flag(
        0x800,
        "Fights mounted",
        "It stays on its mount in combat (CREATURE_TYPE_FLAG_ALLOW_MOUNTED_COMBAT)",
    ),
    flag(
        0x40000,
        "Interaction in combat",
        "The players speak to it while it fights (CREATURE_TYPE_FLAG_ALLOW_INTERACTION_WHILE_IN_COMBAT)",
    ),
    flag(
        0x100000,
        "No name plate",
        "No name plate over it (CREATURE_TYPE_FLAG_NO_NAME_PLATE)",
    ),
    flag(
        0x10000000,
        "Weapons drawn",
        "It never sheathes its weapons (CREATURE_TYPE_FLAG_DO_NOT_SHEATHE)",
    ),
    flag(
        0x40000000,
        "Name not drawn",
        "Its name is not drawn over it (CREATURE_TYPE_FLAG_DO_NOT_RENDER_OBJECT_NAME)",
    ),
];

/// The server's own (`flags_extra`, `CreatureFlagsExtra`); the dungeon boss, set by the server
/// alone, left out.
pub const FLAGS_EXTRA: [Flag; 26] = [
    flag(
        0x1,
        "Binds the instance",
        "Killing it binds the killer's group to the instance (CREATURE_FLAG_EXTRA_INSTANCE_BIND)",
    ),
    flag(
        0x2,
        "Civilian",
        "Never attacks first, whatever the hostility (CREATURE_FLAG_EXTRA_CIVILIAN)",
    ),
    flag(0x4, "No parry", "It never parries (CREATURE_FLAG_EXTRA_NO_PARRY)"),
    flag(
        0x8,
        "No parry haste",
        "Parrying does not hasten its next blow (CREATURE_FLAG_EXTRA_NO_PARRY_HASTEN)",
    ),
    flag(0x10, "No block", "It never blocks (CREATURE_FLAG_EXTRA_NO_BLOCK)"),
    flag(
        0x20,
        "No crushing blows",
        "Its blows never crush (CREATURE_FLAG_EXTRA_NO_CRUSHING_BLOWS)",
    ),
    flag(
        0x40,
        "No experience",
        "Killing it gives no experience (CREATURE_FLAG_EXTRA_NO_XP)",
    ),
    flag(
        0x80,
        "Trigger",
        "An invisible helper of scripts (CREATURE_FLAG_EXTRA_TRIGGER)",
    ),
    flag(
        0x100,
        "No taunt",
        "Taunts have no hold on it (CREATURE_FLAG_EXTRA_NO_TAUNT)",
    ),
    flag(
        0x400,
        "Seen by ghosts alone",
        "Only the dead players see it (CREATURE_FLAG_EXTRA_GHOST_VISIBILITY)",
    ),
    flag(
        0x800,
        "Uses its off hand",
        "It strikes with its off hand weapon too (CREATURE_FLAG_EXTRA_USE_OFFHAND_ATTACK)",
    ),
    flag(
        0x1000,
        "Buys nothing",
        "A vendor the players cannot sell to (CREATURE_FLAG_EXTRA_NO_SELL_VENDOR)",
    ),
    flag(
        0x2000,
        "Never fights",
        "It never enters combat (CREATURE_FLAG_EXTRA_CANNOT_ENTER_COMBAT)",
    ),
    flag(
        0x8000,
        "Guard",
        "A guard: it attacks those who attack its side (CREATURE_FLAG_EXTRA_GUARD)",
    ),
    flag(
        0x10000,
        "Sees through feign death",
        "Feigning death does not fool it (CREATURE_FLAG_EXTRA_IGNORE_FEIGN_DEATH)",
    ),
    flag(
        0x20000,
        "No critical strikes",
        "Its blows are never critical (CREATURE_FLAG_EXTRA_NO_CRIT)",
    ),
    flag(
        0x40000,
        "No skill gains",
        "Fighting it raises no weapon skill (CREATURE_FLAG_EXTRA_NO_SKILL_GAINS)",
    ),
    flag(
        0x80000,
        "Taunt diminishes",
        "Taunts on it diminish (CREATURE_FLAG_EXTRA_OBEYS_TAUNT_DIMINISHING_RETURNS)",
    ),
    flag(
        0x100000,
        "All diminish",
        "Every control on it diminishes, as on a player (CREATURE_FLAG_EXTRA_ALL_DIMINISH)",
    ),
    flag(
        0x200000,
        "Credit without players' damage",
        "Killed by creatures it still counts for the players (CREATURE_FLAG_EXTRA_NO_PLAYER_DAMAGE_REQ)",
    ),
    flag(
        0x400000,
        "Ignores area damage",
        "Spells on an area do not hit it (CREATURE_FLAG_EXTRA_AVOID_AOE)",
    ),
    flag(0x800000, "No dodge", "It never dodges (CREATURE_FLAG_EXTRA_NO_DODGE)"),
    flag(
        0x2000000,
        "Calls no help",
        "It calls no help when attacked (CREATURE_FLAG_EXTRA_DONT_CALL_ASSISTANCE)",
    ),
    flag(
        0x4000000,
        "Answers no call",
        "It does not come when others call for help (CREATURE_FLAG_EXTRA_IGNORE_ALL_ASSISTANCE_CALLS)",
    ),
    flag(
        0x20000000,
        "No pathfinding",
        "It walks straight, through walls (CREATURE_FLAG_EXTRA_IGNORE_PATHFINDING)",
    ),
    flag(
        0x40000000,
        "Immune to knockback",
        "Knockbacks do not move it (CREATURE_FLAG_EXTRA_IMMUNITY_KNOCKBACK)",
    ),
];

/// Its rank (`rank`, `CreatureEliteType`).
pub const RANKS: [Choice; 5] = [
    (0, "Normal", "No dragon around its portrait (CREATURE_ELITE_NORMAL)"),
    (
        1,
        "Elite",
        "A gold dragon around its portrait, stronger (CREATURE_ELITE_ELITE)",
    ),
    (4, "Rare", "A silver dragon around its portrait (CREATURE_ELITE_RARE)"),
    (
        2,
        "Rare elite",
        "A silver winged dragon around its portrait (CREATURE_ELITE_RAREELITE)",
    ),
    (
        3,
        "Boss",
        "A world boss: its level shown as ?? (CREATURE_ELITE_WORLDBOSS)",
    ),
];

/// Its class, which sets its health and mana by level (`unit_class`): AzerothCore knows four.
pub const CLASSES: [Choice; 4] = [
    (1, "Warrior", "Health, no mana"),
    (2, "Paladin", "Health and some mana"),
    (4, "Rogue", "Health, no mana"),
    (8, "Mage", "Less health, much mana"),
];

/// The expansion its health and damage by level are taken from (`exp`).
pub const EXPANSIONS: [Choice; 3] = [
    (0, "Classic", "The stats of the creatures of the first game"),
    (1, "The Burning Crusade", "The stats of the creatures of Outland"),
    (2, "Wrath of the Lich King", "The stats of the creatures of Northrend"),
];

/// Its kind (`type`, `CreatureType`).
pub const TYPES: [Choice; 14] = [
    (0, "None", "No kind"),
    (1, "Beast", "A beast: skinned, tamed with a family"),
    (2, "Dragonkin", "A dragon or whelp"),
    (3, "Demon", "A demon: banished, enslaved"),
    (4, "Elemental", "An elemental: banished"),
    (5, "Giant", "A giant"),
    (6, "Undead", "An undead: turned, shackled"),
    (7, "Humanoid", "A humanoid: pickpocketed, sapped"),
    (8, "Critter", "A critter: harmless, no experience"),
    (9, "Mechanical", "A machine"),
    (10, "Not specified", "Kind not specified"),
    (11, "Totem", "A totem"),
    (12, "Non-combat pet", "A companion pet"),
    (13, "Gas cloud", "A cloud of gas"),
];

/// Its family, for a beast (`family`, `CreatureFamily`).
pub const FAMILIES: [Choice; 41] = [
    (0, "None", "No family"),
    (1, "Wolf", ""),
    (2, "Cat", ""),
    (3, "Spider", ""),
    (4, "Bear", ""),
    (5, "Boar", ""),
    (6, "Crocolisk", ""),
    (7, "Carrion bird", ""),
    (8, "Crab", ""),
    (9, "Gorilla", ""),
    (11, "Raptor", ""),
    (12, "Tallstrider", ""),
    (15, "Felhunter", "A warlock's demon"),
    (16, "Voidwalker", "A warlock's demon"),
    (17, "Succubus", "A warlock's demon"),
    (19, "Doomguard", "A warlock's demon"),
    (20, "Scorpid", ""),
    (21, "Turtle", ""),
    (23, "Imp", "A warlock's demon"),
    (24, "Bat", ""),
    (25, "Hyena", ""),
    (26, "Bird of prey", ""),
    (27, "Wind serpent", ""),
    (28, "Remote control", ""),
    (29, "Felguard", "A warlock's demon"),
    (30, "Dragonhawk", ""),
    (31, "Ravager", ""),
    (32, "Warp stalker", ""),
    (33, "Sporebat", ""),
    (34, "Nether ray", ""),
    (35, "Serpent", ""),
    (37, "Moth", ""),
    (38, "Chimaera", "Exotic"),
    (39, "Devilsaur", "Exotic"),
    (40, "Ghoul", "A death knight's ghoul"),
    (41, "Silithid", "Exotic"),
    (42, "Worm", "Exotic"),
    (43, "Rhino", "Exotic"),
    (44, "Wasp", ""),
    (45, "Core hound", "Exotic"),
    (46, "Spirit beast", "Exotic"),
];

/// The school of its blows (`dmgschool`).
pub const SCHOOLS: [Choice; 7] = [
    (0, "Physical", "Its blows are physical, lessened by armour"),
    (1, "Holy", ""),
    (2, "Fire", ""),
    (3, "Nature", ""),
    (4, "Frost", ""),
    (5, "Shadow", ""),
    (6, "Arcane", ""),
];

/// How it moves where it stands (`MovementType`).
pub const MOVEMENTS: [Choice; 3] = [
    (0, "Stands", "It stays at its spawn point"),
    (
        1,
        "Wanders",
        "It wanders about its spawn point, as far as its spawn says",
    ),
    (2, "Walks a path", "It walks the path its spawn names"),
];

/// A number of the sheet: its column, its label, what it does, and whether it takes decimals.
pub struct Number {
    pub column: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub real: bool,
}

const fn whole(column: &'static str, label: &'static str, hint: &'static str) -> Number {
    Number {
        column,
        label,
        hint,
        real: false,
    }
}

const fn real(column: &'static str, label: &'static str, hint: &'static str) -> Number {
    Number {
        column,
        label,
        hint,
        real: true,
    }
}

/// Its strength, multiplied from what its level and class give.
pub const STRENGTH: [Number; 7] = [
    real(
        "HealthModifier",
        "Health ×",
        "Its health: that of its level and class, times this",
    ),
    real(
        "ManaModifier",
        "Mana ×",
        "Its mana: that of its level and class, times this",
    ),
    real("ArmorModifier", "Armour ×", "Its armour: that of its level, times this"),
    real(
        "DamageModifier",
        "Damage ×",
        "Its blows: those of its level, times this",
    ),
    real(
        "ExperienceModifier",
        "Experience ×",
        "The experience it gives: that of its level, times this",
    ),
    whole(
        "BaseAttackTime",
        "Attack time (ms)",
        "The milliseconds between two of its blows in melee",
    ),
    whole(
        "RangeAttackTime",
        "Ranged attack time (ms)",
        "The milliseconds between two of its shots",
    ),
];

/// How fast it goes, and how far it sees.
pub const MOVEMENT: [Number; 6] = [
    real("speed_walk", "Walk speed", "1 is a player's walk"),
    real("speed_run", "Run speed", "1.14286 is a player's run"),
    real("speed_swim", "Swim speed", "1 is the usual"),
    real("speed_flight", "Flight speed", "1 is the usual"),
    real("HoverHeight", "Hover height", "How high it floats when it hovers"),
    real(
        "detection_range",
        "Aggro range (yd)",
        "How far it sees the players it attacks",
    ),
];

/// What it leaves when it dies.
pub const LOOT: [Number; 3] = [
    whole(
        "lootid",
        "Loot",
        "The entry of its loot in creature_loot_template; 0 for none",
    ),
    whole(
        "pickpocketloot",
        "Pickpocket loot",
        "The entry of what a rogue takes from its pockets (pickpocketing_loot_template); 0 for none",
    ),
    whole(
        "skinloot",
        "Skinning loot",
        "The entry of what its corpse gives skinned (skinning_loot_template); 0 for none",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_flag_is_one_bit_of_its_own_and_says_what_it_does() {
        for flags in [
            &NPC_FLAGS[..],
            &UNIT_FLAGS[..],
            &UNIT_FLAGS2[..],
            &TYPE_FLAGS[..],
            &FLAGS_EXTRA[..],
        ] {
            let mut seen = 0u32;
            for flag in flags {
                assert_eq!(flag.bit.count_ones(), 1, "{}", flag.name);
                assert_eq!(seen & flag.bit, 0, "{} twice", flag.name);
                assert!(!flag.hint.is_empty(), "{}", flag.name);
                seen |= flag.bit;
            }
        }
        for choices in [
            &RANKS[..],
            &CLASSES[..],
            &EXPANSIONS[..],
            &TYPES[..],
            &FAMILIES[..],
            &SCHOOLS[..],
            &MOVEMENTS[..],
        ] {
            let mut values: Vec<i64> = choices.iter().map(|(value, _, _)| *value).collect();
            values.sort();
            values.dedup();
            assert_eq!(values.len(), choices.len());
        }
    }
}
