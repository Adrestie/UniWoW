//! Interface of the "weather" service: the weather the view is drawn under, as the module `weather`
//! applies it, the server's or one chosen in the editor; read by the light of the map, which mixes
//! in the params of storm of its lights by its intensity. And the weathers of the server as
//! AzerothCore names them (`WeatherState`), which its observer sends.

use std::sync::Arc;

use crate::ServiceKey;

/// Provide with `Registrar::provide(SERVICE, …)`, ask with `Context::service(SERVICE)`.
pub const SERVICE: ServiceKey<Handle> = ServiceKey::new("weather");

pub type Handle = Arc<dyn Weather>;

/// Shared between threads (rule T3).
pub trait Weather: Send + Sync {
    /// The weather drawn now; none for fine weather.
    fn current(&self) -> Option<Effect>;
}

/// A weather: its kind and its intensity, from 0 to 1, the grade the server sends with it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Effect {
    pub kind: Kind,
    pub intensity: f32,
}

/// The kinds of weather of 3.3.5a.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Fog,
    Rain,
    Snow,
    Sandstorm,
    Thunders,
    BlackRain,
    BlackSnow,
}

impl Kind {
    /// Every kind, in the order the panel lists them.
    pub const ALL: [Kind; 7] = [
        Kind::Rain,
        Kind::Snow,
        Kind::Sandstorm,
        Kind::Thunders,
        Kind::BlackRain,
        Kind::BlackSnow,
        Kind::Fog,
    ];

    /// Its name in words.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Fog => "fog",
            Kind::Rain => "rain",
            Kind::Snow => "snow",
            Kind::Sandstorm => "sandstorm",
            Kind::Thunders => "thunderstorm",
            Kind::BlackRain => "black rain",
            Kind::BlackSnow => "black snow",
        }
    }

    /// Whether it falls as rain, snow or sand around the eye: all but the fog.
    pub fn falls(self) -> bool {
        self != Kind::Fog
    }

    /// Whether it brings the params of storm of the lights: all that falls.
    pub fn stormy(self) -> bool {
        self.falls()
    }

    /// Whether it thickens the fog: the fog and the sandstorm.
    pub fn foggy(self) -> bool {
        matches!(self, Kind::Fog | Kind::Sandstorm)
    }
}

/// The states of the weather AzerothCore sends its players (`WeatherState`, `Weather.h`): fine (0),
/// fog (1), light, medium and heavy rain (3, 4, 5), snow (6, 7, 8) and sandstorm (22, 41, 42),
/// thunders (86), black rain (90) and black snow (106).
pub mod state {
    pub const FINE: u32 = 0;
    pub const FOG: u32 = 1;
    pub const LIGHT_RAIN: u32 = 3;
    pub const MEDIUM_RAIN: u32 = 4;
    pub const HEAVY_RAIN: u32 = 5;
    pub const LIGHT_SNOW: u32 = 6;
    pub const MEDIUM_SNOW: u32 = 7;
    pub const HEAVY_SNOW: u32 = 8;
    pub const LIGHT_SANDSTORM: u32 = 22;
    pub const MEDIUM_SANDSTORM: u32 = 41;
    pub const HEAVY_SANDSTORM: u32 = 42;
    pub const THUNDERS: u32 = 86;
    pub const BLACK_RAIN: u32 = 90;
    pub const BLACK_SNOW: u32 = 106;
}

impl Effect {
    /// The weather of the state `state` of AzerothCore and its grade `grade`: none for fine weather
    /// or a state unknown; its intensity the grade, held within 0 and 1.
    pub fn of_state(state: u32, grade: f32) -> Option<Effect> {
        let kind = match state {
            state::FOG => Kind::Fog,
            state::LIGHT_RAIN | state::MEDIUM_RAIN | state::HEAVY_RAIN => Kind::Rain,
            state::LIGHT_SNOW | state::MEDIUM_SNOW | state::HEAVY_SNOW => Kind::Snow,
            state::LIGHT_SANDSTORM | state::MEDIUM_SANDSTORM | state::HEAVY_SANDSTORM => Kind::Sandstorm,
            state::THUNDERS => Kind::Thunders,
            state::BLACK_RAIN => Kind::BlackRain,
            state::BLACK_SNOW => Kind::BlackSnow,
            _ => return None,
        };
        let intensity = if grade.is_finite() { grade.clamp(0.0, 1.0) } else { 0.0 };
        Some(Effect { kind, intensity })
    }

    /// The name of the state `state` of AzerothCore, in words; none for a state unknown.
    pub fn state_name(state: u32) -> Option<&'static str> {
        Some(match state {
            state::FINE => "fine",
            state::FOG => "fog",
            state::LIGHT_RAIN => "light rain",
            state::MEDIUM_RAIN => "medium rain",
            state::HEAVY_RAIN => "heavy rain",
            state::LIGHT_SNOW => "light snow",
            state::MEDIUM_SNOW => "medium snow",
            state::HEAVY_SNOW => "heavy snow",
            state::LIGHT_SANDSTORM => "light sandstorm",
            state::MEDIUM_SANDSTORM => "medium sandstorm",
            state::HEAVY_SANDSTORM => "heavy sandstorm",
            state::THUNDERS => "thunders",
            state::BLACK_RAIN => "black rain",
            state::BLACK_SNOW => "black snow",
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Effect, Kind, state};

    #[test]
    fn the_states_of_azerothcore_are_read_as_their_kind_their_grade_the_intensity() {
        assert_eq!(Effect::of_state(state::FINE, 0.9), None);
        assert_eq!(Effect::of_state(2, 0.5), None, "a state unknown");
        for (state, kind) in [
            (state::FOG, Kind::Fog),
            (state::LIGHT_RAIN, Kind::Rain),
            (state::HEAVY_RAIN, Kind::Rain),
            (state::MEDIUM_SNOW, Kind::Snow),
            (state::LIGHT_SANDSTORM, Kind::Sandstorm),
            (state::HEAVY_SANDSTORM, Kind::Sandstorm),
            (state::THUNDERS, Kind::Thunders),
            (state::BLACK_RAIN, Kind::BlackRain),
            (state::BLACK_SNOW, Kind::BlackSnow),
        ] {
            assert_eq!(
                Effect::of_state(state, 0.5).map(|effect| effect.kind),
                Some(kind),
                "{state}"
            );
        }
        assert_eq!(Effect::of_state(state::HEAVY_RAIN, 1.5).unwrap().intensity, 1.0);
        assert_eq!(Effect::of_state(state::HEAVY_RAIN, f32::NAN).unwrap().intensity, 0.0);
        assert_eq!(Effect::state_name(state::MEDIUM_SANDSTORM), Some("medium sandstorm"));
        assert_eq!(Effect::state_name(2), None);
        assert!(Kind::Rain.falls() && Kind::Rain.stormy() && !Kind::Rain.foggy());
        assert!(!Kind::Fog.falls() && Kind::Fog.foggy() && Kind::Sandstorm.foggy());
    }
}
