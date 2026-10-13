//! Tests of the sounds: files the tests write decoded; the engine playing into a mixer the tests
//! read, as the device would, at one channel and 1,000 frames a second, so that a frame is a
//! millisecond.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use rodio::mixer::{self, MixerSource};
use rodio::{ChannelCount, SampleRate};

use crate::{BLOCK, Category, Clip, Engine, Output, Sound, SoundId, State};

/// A WAV of 16-bit PCM of `channels` channels at `rate`.
pub fn wav(channels: u16, rate: u32, samples: &[i16]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    bytes.extend_from_slice(&(channels * 2).to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

/// An MP3 of `frames` silent frames: MPEG-1 layer III, 128 kb/s, 44,100 Hz, one channel, each
/// frame its header and 413 bytes of nothing.
fn silent_mp3(frames: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 0..frames {
        bytes.extend_from_slice(&[0xFF, 0xFB, 0x90, 0xC0]);
        bytes.extend(std::iter::repeat_n(0u8, 413));
    }
    bytes
}

#[test]
fn a_wav_and_an_mp3_are_decoded_and_what_is_not_a_sound_refused() {
    let clip = Clip::decode(wav(1, 8_000, &[0, 16_384, -16_384, 32_767])).unwrap();
    assert_eq!((clip.channels(), clip.rate()), (1, 8_000));
    let expected = [0.0, 0.5, -0.5, 1.0];
    assert!(
        clip.samples()
            .iter()
            .zip(expected)
            .all(|(got, want)| (got - want).abs() < 1e-3),
        "{:?}",
        clip.samples()
    );
    assert_eq!(clip.duration(), Duration::from_micros(500));
    let stereo = Clip::decode(wav(2, 22_050, &[100, -100, 200, -200])).unwrap();
    assert_eq!((stereo.channels(), stereo.samples().len()), (2, 4));
    assert!(stereo.samples()[0] > 0.0 && stereo.samples()[1] < 0.0, "interleaved");
    let mp3 = Clip::decode(silent_mp3(20)).unwrap();
    assert_eq!((mp3.channels(), mp3.rate()), (1, 44_100));
    assert!(mp3.samples().len() >= 1_152 * 10, "{}", mp3.samples().len());
    assert!(mp3.samples().iter().all(|sample| sample.abs() < 1e-6), "silent");
    assert!(Clip::decode(b"not a sound at all, not even close".to_vec()).is_err());
    assert!(Clip::decode(wav(1, 8_000, &[])).is_err(), "no sample");
    assert!(Clip::new(0, 8_000, vec![0.0]).is_err());
    assert!(Clip::new(2, 8_000, vec![0.0; 3]).is_err(), "a frame cut short");
}

/// An engine playing into a mixer of one channel at 1,000 frames a second, read by the test, the
/// output open; the output's opening held until `release` is sent when `held`.
fn engine(held: bool) -> (Engine, MixerSource, mpsc::Sender<()>) {
    let (mixer, source) = mixer::mixer(ChannelCount::new(1).unwrap(), SampleRate::new(1_000).unwrap());
    let (release, wait) = mpsc::channel::<()>();
    let engine = Engine::with_output(Box::new(move || {
        if held {
            wait.recv().map_err(|why| why.to_string())?;
        }
        Ok(Output {
            mixer,
            device: "the test's".to_owned(),
            keep: Box::new(()),
        })
    }));
    if !held {
        settled(&engine);
    }
    (engine, source, release)
}

/// The state of `engine` once its output is open or refused.
fn settled(engine: &Engine) -> State {
    let since = Instant::now();
    while engine.state() == State::Opening {
        assert!(since.elapsed() < Duration::from_secs(10), "the output never opens");
        std::thread::yield_now();
    }
    engine.state()
}

/// A clip of `samples` at one channel and 1,000 frames a second.
fn clip(samples: &[f32]) -> Clip {
    Clip::new(1, 1_000, samples.to_vec()).unwrap()
}

fn sound(samples: &[f32], category: Category, volume: f32, looped: bool, fade: u64) -> Sound {
    Sound {
        clip: clip(samples),
        category,
        volume,
        looped,
        fade: Duration::from_millis(fade),
    }
}

/// The next `count` samples of the mixer, none where it has nothing to play.
fn take(source: &mut MixerSource, count: usize) -> Vec<Option<f32>> {
    (0..count).map(|_| source.next()).collect()
}

fn near(got: Option<f32>, want: f32) -> bool {
    got.is_some_and(|got| (got - want).abs() < 1e-4)
}

#[test]
fn a_sound_plays_at_its_volume_times_its_category_and_the_master_volume_muted_at_once() {
    let (engine, mut source, _) = engine(false);
    assert_eq!(engine.state(), State::Playing("the test's".to_owned()));
    let id = engine.play(sound(&[0.5], Category::Ambience, 0.8, true, 0));
    assert!(engine.playing(id));
    assert_eq!(engine.count(), 1);
    let first = take(&mut source, BLOCK);
    assert!(first.iter().all(|sample| near(*sample, 0.4)), "{first:?}");
    // The ambience at half: over the next block, from the whole to half, by a straight line.
    engine.set_category(Category::Ambience, 0.5);
    let moving = take(&mut source, BLOCK);
    assert!(near(moving[0], 0.4) && near(moving[BLOCK / 2], 0.3), "{moving:?}");
    assert!(
        moving
            .windows(2)
            .all(|pair| pair[1].unwrap() <= pair[0].unwrap() + 1e-6)
    );
    assert!(take(&mut source, BLOCK).iter().all(|sample| near(*sample, 0.2)));
    // The effects alone at 0: the ambience as it was.
    engine.set_category(Category::Effects, 0.0);
    take(&mut source, BLOCK);
    assert!(take(&mut source, BLOCK).iter().all(|sample| near(*sample, 0.2)));
    // The master volume at a quarter; muted; heard again.
    engine.set_master(0.25);
    take(&mut source, BLOCK);
    assert!(take(&mut source, BLOCK).iter().all(|sample| near(*sample, 0.05)));
    engine.set_muted(true);
    take(&mut source, BLOCK);
    assert!(take(&mut source, BLOCK).iter().all(|sample| near(*sample, 0.0)));
    engine.set_muted(false);
    take(&mut source, BLOCK);
    assert!(take(&mut source, BLOCK).iter().all(|sample| near(*sample, 0.05)));
    // Within 0 and 1, whatever it is given.
    engine.set_master(7.0);
    engine.set_category(Category::Ambience, f32::NAN);
    take(&mut source, BLOCK);
    assert!(take(&mut source, BLOCK).iter().all(|sample| near(*sample, 0.0)));
    engine.set_category(Category::Ambience, 1.0);
    take(&mut source, BLOCK);
    assert!(take(&mut source, BLOCK).iter().all(|sample| near(*sample, 0.4)));
}

#[test]
fn a_sound_fades_in_and_to_another_volume_and_stops_once_faded_out() {
    let (engine, mut source, _) = engine(false);
    // Faded in over 100 ms, a frame a millisecond: a tenth of the way every 10 frames.
    let id = engine.play(sound(&[1.0], Category::Effects, 1.0, true, 100));
    let rising = take(&mut source, 120);
    assert!(
        near(rising[0], 0.0) && near(rising[50], 0.5) && near(rising[100], 1.0),
        "{rising:?}"
    );
    assert!(near(rising[119], 1.0));
    take(&mut source, BLOCK - 120);
    // To 0.5 over 200 ms, from the next block on.
    engine.set_volume(id, 0.5, Duration::from_millis(200));
    let falling = take(&mut source, BLOCK);
    assert!(
        near(falling[0], 1.0) && near(falling[100], 0.75) && near(falling[200], 0.5),
        "{falling:?}"
    );
    assert!(near(falling[BLOCK - 1], 0.5));
    // Stopped over 100 ms: out, then nothing left to play, no longer playing.
    engine.stop(id, Duration::from_millis(100));
    assert!(!engine.playing(id), "stopped");
    let stopping = take(&mut source, BLOCK);
    assert!(near(stopping[50], 0.25) && near(stopping[99], 0.005), "{stopping:?}");
    assert!(stopping[100..].iter().all(Option::is_none), "{stopping:?}");
    assert_eq!(engine.count(), 0);
    // A sound unknown: nothing.
    engine.set_volume(SoundId(999), 1.0, Duration::ZERO);
    engine.stop(SoundId(999), Duration::ZERO);
}

#[test]
fn a_sound_loops_or_ends_with_its_clip() {
    let (engine, mut source, _) = engine(false);
    let once = engine.play(sound(&[0.1, 0.2, 0.3], Category::Effects, 1.0, false, 0));
    let played = take(&mut source, 5);
    assert!(
        near(played[0], 0.1) && near(played[2], 0.3) && played[3].is_none(),
        "{played:?}"
    );
    assert!(!engine.playing(once), "ended with its clip");
    let looped = engine.play(sound(&[0.1, 0.2, 0.3], Category::Effects, 1.0, true, 0));
    let played = take(&mut source, 7);
    assert!(near(played[3], 0.1) && near(played[6], 0.1), "{played:?}");
    assert!(engine.playing(looped));
    // Two at once, added.
    engine.play(sound(&[0.5], Category::Music, 0.5, false, 0));
    let both = take(&mut source, 1);
    assert!(near(both[0], 0.2 + 0.25), "{both:?}");
}

#[test]
fn the_sounds_played_before_the_device_opens_play_once_it_is_and_none_without_one() {
    let (engine, mut source, release) = engine(true);
    assert_eq!(engine.state(), State::Opening);
    let early = engine.play(sound(&[0.5], Category::Effects, 1.0, true, 0));
    let stopped = engine.play(sound(&[0.25], Category::Effects, 1.0, true, 0));
    engine.stop(stopped, Duration::ZERO);
    assert!(engine.playing(early) && !engine.playing(stopped));
    release.send(()).unwrap();
    assert_eq!(settled(&engine), State::Playing("the test's".to_owned()));
    // The one stopped meanwhile never heard.
    let played = take(&mut source, 2 * BLOCK);
    assert!(played.iter().all(|sample| near(*sample, 0.5)), "{:?}", &played[..4]);
    // Without a device: nothing played, the reason said, nothing waited for.
    let silent = Engine::with_output(Box::new(|| Err("no device here".to_owned())));
    assert_eq!(settled(&silent), State::Silent("no device here".to_owned()));
    let id = silent.play(sound(&[0.5], Category::Effects, 1.0, true, 0));
    assert!(!silent.playing(id));
    assert_eq!(silent.count(), 0);
    silent.set_volume(id, 0.5, Duration::ZERO);
    silent.stop(id, Duration::ZERO);
    // Shut down: every sound stopped, nothing played after, said.
    engine.shutdown();
    assert_eq!(engine.state(), State::Silent("stopped".to_owned()));
    assert!(!engine.playing(early));
    take(&mut source, BLOCK);
    assert!(take(&mut source, 4).iter().all(Option::is_none));
    let after = engine.play(sound(&[0.5], Category::Effects, 1.0, true, 0));
    assert!(!engine.playing(after));
    assert!(take(&mut source, 4).iter().all(Option::is_none));
    engine.shutdown();
}
