//! The call's voice: µ-law samples, a resampler, and the two audio backends a
//! call stands on — a port of the web build's audio path, voice only.
//!
//! # The format is the wire's
//!
//! A call carries G.711 µ-law — 8kHz, mono, 20ms frames of 160 bytes — because
//! that is what the web and Android calls send, and a desktop caller must speak
//! the same codec into the same track. The codec here is the CCITT reference
//! implementation in pure Rust; the frame pacing lives in `net::call`, which
//! hands each 160-byte frame to WebRTC with a 20ms duration so the RTP
//! timestamps come out right.
//!
//! # The backends are a platform fact, not a preference
//!
//! Linux: ALSA, opened at runtime with `libloading`. A compile-time binding
//! would need `alsa-sys` to find `libasound` headers, which the shared CI host
//! does not install — and dlopen of `libasound.so.2` needs none. ALSA is asked
//! for exactly 8000Hz mono through the `default` (plug) device, and either
//! grants it or refuses the open.
//!
//! Windows and macOS: cpal, on the device's *native* default configuration —
//! WASAPI and CoreAudio will not build a stream at 8000Hz, so the stream runs
//! at the device's rate and the [`Resampler`] moves samples the rest of the
//! way in the call's pump tasks.
//!
//! # One interface for both
//!
//! A [`Microphone`] yields chunks of mono `i16` at its own `rate` and a
//! [`Speaker`] accepts them — anything platform-shaped stays behind those two.
//! Both stop when dropped, and a teardown that forgets a call must never leave
//! a microphone open.
//!
//! # What is deliberately not here
//!
//! Device selection, echo cancellation, and level meters. The default devices
//! and the OS's own cancellation are what every other Migo client uses, and the
//! one honest failure mode — "Microphone unavailable" — is surfaced rather than
//! papered over with a silent stream of zeros.

use std::fmt;
use std::sync::mpsc as std_mpsc;

/// The wire's audio rate: 8000Hz, the rate µ-law is defined at.
pub const CALL_SAMPLE_RATE: u32 = 8_000;

/// One 20ms frame of call audio: 160 µ-law bytes.
pub const FRAME_SAMPLES: usize = 160;

/// The µ-law codec's bias, the CCITT reference's own constant.
const BIAS: i32 = 0x84;

/// The µ-law codec's clip level, below i16::MAX by design.
const CLIP: i32 = 32_635;

/// Something in the audio path could not be opened or kept running.
///
/// The message is user-facing ("Microphone unavailable: …") because the one
/// thing a caller can do about it is check the device — an errno in a toast
/// helps nobody.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallAudioError(String);

impl fmt::Display for CallAudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CallAudioError {}

fn unavailable(what: &str, detail: impl fmt::Display) -> CallAudioError {
    CallAudioError(format!("{what} unavailable: {detail}"))
}

// --- µ-law, the CCITT G.711 reference in pure Rust ---

/// The exponent table: which of µ-law's eight segments an amplitude falls in,
/// indexed by the top eight bits of the biased magnitude.
const EXPONENT: [u8; 256] = exp_table();

/// The CCITT reference's `seg_end` logic as a const loop: index i maps to the
/// segment whose range contains it — 0..2 → 0, 2..4 → 1, doubling each time.
const fn exp_table() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = if i < 2 {
            0
        } else if i < 4 {
            1
        } else if i < 8 {
            2
        } else if i < 16 {
            3
        } else if i < 32 {
            4
        } else if i < 64 {
            5
        } else if i < 128 {
            6
        } else {
            7
        };
        i += 1;
    }
    table
}

/// Encodes one linear sample as µ-law. The output is complemented, so silence
/// is `0xFF` — the same byte the reference encoder writes for a zero sample.
#[must_use]
pub fn linear_to_ulaw(sample: i16) -> u8 {
    let mut magnitude = sample as i32;
    let sign = (magnitude >> 8) & 0x80;
    if sign != 0 {
        magnitude = -magnitude;
    }
    if magnitude > CLIP {
        magnitude = CLIP;
    }
    magnitude += BIAS;
    let exponent = EXPONENT[((magnitude >> 7) & 0xFF) as usize] as i32;
    let mantissa = (magnitude >> (exponent + 3)) & 0x0F;
    (!(sign | (exponent << 4) | mantissa)) as u8
}

/// Decodes one µ-law byte back to linear, the reference decoder's own arithmetic.
#[must_use]
pub fn ulaw_to_linear(u: u8) -> i16 {
    let u = !u;
    let magnitude = (((u & 0x0F) as i32) << 3) + BIAS;
    let magnitude = magnitude << ((u & 0x70) >> 4);
    let linear = if u & 0x80 != 0 {
        BIAS - magnitude
    } else {
        magnitude - BIAS
    };
    linear as i16
}

/// Encodes a run of linear samples as a µ-law frame.
#[must_use]
pub fn ulaw_encode(samples: &[i16]) -> Vec<u8> {
    samples.iter().map(|&s| linear_to_ulaw(s)).collect()
}

/// Decodes a µ-law frame back to linear samples.
#[must_use]
pub fn ulaw_decode(bytes: &[u8]) -> Vec<i16> {
    bytes.iter().map(|&b| ulaw_to_linear(b)).collect()
}

// --- the resampler ---

/// A linear-interpolation resampler that works on a stream, not a buffer.
///
/// The call's pumps feed it chunks of arbitrary length as they arrive; the
/// carried state — where the next output sample sits between the previous
/// chunk's last sample and the next chunk's first — is what makes two chunks
/// resample exactly as one long buffer would. A resampler constructed
/// `from == to` is a pass-through in all but name and is never constructed.
pub struct Resampler {
    /// Input samples per output sample: `from / to`.
    step: f64,
    /// Where the next output sample sits, in input samples since this chunk's
    /// start. Enters a chunk in `[-1, n)` — the `-1` tail is the interpolation
    /// the previous chunk could not finish without this chunk's first sample.
    pos: f64,
    /// The previous chunk's last sample, the left side of that interpolation.
    prev: i16,
}

impl Resampler {
    /// Resamples `from` Hz to `to` Hz.
    #[must_use]
    pub fn new(from: u32, to: u32) -> Self {
        Self {
            step: f64::from(from) / f64::from(to),
            pos: 0.0,
            prev: 0,
        }
    }

    /// Resamples one chunk of the stream into `output`, carrying what cannot be
    /// finished yet. An output sample can only be written once its right
    /// neighbour exists, so the last interval of every chunk waits for the next
    /// chunk — an empty next chunk still counts as one (silence is a sample
    /// too), which is why the pumps only construct a resampler for a stream
    /// that will keep flowing.
    pub fn process(&mut self, input: &[i16], output: &mut Vec<i16>) {
        if input.is_empty() {
            return;
        }
        let n = input.len() as f64;
        // An output can be written while its right neighbour is inside this
        // chunk: floor(pos) + 1 <= n - 1, i.e. pos < n - 1.
        while self.pos < n - 1.0 {
            let index = self.pos.floor() as i64;
            let (left, right) = if index < 0 {
                (f64::from(self.prev), f64::from(input[0]))
            } else {
                let i = index as usize;
                (f64::from(input[i]), f64::from(input[i + 1]))
            };
            let frac = self.pos - index as f64;
            output.push((left + (right - left) * frac).round() as i16);
            self.pos += self.step;
        }
        self.pos -= n;
        self.prev = input[input.len() - 1];
    }
}

// --- the two audio backends' shared shapes ---

/// The platform handle that keeps a stream alive. Dropping it stops the stream;
/// the field order in the structs below is load-bearing only in that the
/// channel ends drop with it too.
enum MicrophoneGuard {
    /// The ALSA capture thread; exits when the receiver it feeds is dropped.
    #[cfg(target_os = "linux")]
    Alsa(std::thread::JoinHandle<()>),
    /// The cpal input stream, which cpal stops when dropped.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    Cpal(cpal::Stream),
}

/// An open microphone: mono `i16` chunks at [`Microphone::rate`], stopped when
/// dropped. A microphone that dies mid-call simply goes quiet — the capture
/// thread exits, the channel closes, and the call goes on one-sided rather than
/// dying over a device hiccup the user can hear nothing of.
pub struct Microphone {
    /// The chunks this device's input becomes.
    pub frames: std_mpsc::Receiver<Vec<i16>>,
    /// The rate those chunks arrive at. ALSA grants the requested 8000; the
    /// cpal hosts give the device's native rate, and the caller resamples.
    pub rate: u32,
    _guard: MicrophoneGuard,
}

/// An open speaker: accepts mono `i16` chunks at [`Speaker::rate`], stopped
/// when dropped. Chunks sent after an internal failure are quietly dropped —
/// half a second of missing audio is a blip; tearing down a live call over it
/// is a decision nobody made.
pub struct Speaker {
    /// Where the pump sends what the far end said.
    pub frames: std_mpsc::Sender<Vec<i16>>,
    /// The rate those chunks must arrive at.
    pub rate: u32,
    _guard: SpeakerGuard,
}

impl Microphone {
    /// Hands the chunk receiver to the capture pump, leaving a dead one in its place so the
    /// guard struct stays whole and droppable by the call engine.
    ///
    /// The engine must keep *this* struct for as long as the call runs — dropping it is what
    /// stops capture — while the pump needs the receiver by value. A partial move out would
    /// make the struct unreturnable, so the receiver is swapped for a channel whose sender is
    /// dropped on the spot: a receiver that can only ever read "disconnected", which is the
    /// same answer a dead microphone gives.
    pub fn take_frames(&mut self) -> std_mpsc::Receiver<Vec<i16>> {
        let (dead_tx, dead_rx) = std_mpsc::channel();
        drop(dead_tx);
        std::mem::replace(&mut self.frames, dead_rx)
    }
}

enum SpeakerGuard {
    #[cfg(target_os = "linux")]
    Alsa(std::thread::JoinHandle<()>),
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    Cpal(cpal::Stream),
}

/// Opens the default microphone.
pub fn open_microphone() -> Result<Microphone, CallAudioError> {
    #[cfg(target_os = "linux")]
    return alsa::open_microphone();
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    return cpal_backend::open_microphone();
}

/// Opens the default speaker.
pub fn open_speaker() -> Result<Speaker, CallAudioError> {
    #[cfg(target_os = "linux")]
    return alsa::open_speaker();
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    return cpal_backend::open_speaker();
}

// --- the ALSA backend, dlopened at runtime ---

#[cfg(target_os = "linux")]
mod alsa {
    use super::{unavailable, CallAudioError, Microphone, Speaker};
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int, c_long, c_uint, c_ulong, c_void};
    use std::sync::mpsc as std_mpsc;
    use std::sync::Arc;
    use std::thread::JoinHandle;

    /// A PCM handle, opaque here exactly as it is in `alsa/asoundlib.h`.
    type Pcm = *mut c_void;

    // The wire constants of `alsa/asoundlib.h` this module relies on.
    const STREAM_PLAYBACK: c_int = 0;
    const STREAM_CAPTURE: c_int = 1;
    const FORMAT_S16_LE: c_uint = 2;
    const ACCESS_RW_INTERLEAVED: c_uint = 3;
    /// 50ms of buffering: long enough for the scheduler, short enough for a call.
    const LATENCY_US: c_uint = 50_000;
    /// One blocking read: 160 frames = one 20ms µ-law frame's worth.
    const READ_FRAMES: c_ulong = 160;

    /// The ALSA entry points this module uses, loaded from `libasound.so.2` at
    /// runtime. Raw function pointers are copied *out* of libloading's `Symbol`
    /// borrows; the `Library` lives in the same struct and is never dropped
    /// before them, which is the invariant that makes those copies sound.
    struct Alsa {
        #[allow(dead_code)] // dropped last by declaration order, never read
        _library: libloading::Library,
        open: unsafe extern "C" fn(*mut Pcm, *const c_char, c_int, c_int) -> c_int,
        close: unsafe extern "C" fn(Pcm) -> c_int,
        set_params:
            unsafe extern "C" fn(Pcm, c_uint, c_uint, c_uint, c_uint, c_int, c_uint) -> c_int,
        readi: unsafe extern "C" fn(Pcm, *mut c_void, c_ulong) -> c_long,
        writei: unsafe extern "C" fn(Pcm, *const c_void, c_ulong) -> c_long,
        recover: unsafe extern "C" fn(Pcm, c_int, c_int) -> c_int,
        drain: unsafe extern "C" fn(Pcm) -> c_int,
    }

    impl Alsa {
        fn load() -> Result<Self, CallAudioError> {
            unsafe {
                let library = libloading::Library::new("libasound.so.2")
                    .map_err(|error| unavailable("Microphone and speaker", error))?;
                macro_rules! symbol {
                    ($name:literal) => {
                        *library
                            .get(concat!($name, "\0").as_bytes())
                            .map_err(|error| unavailable("the system audio library", error))?
                    };
                }
                Ok(Self {
                    open: symbol!("snd_pcm_open"),
                    close: symbol!("snd_pcm_close"),
                    set_params: symbol!("snd_pcm_set_params"),
                    readi: symbol!("snd_pcm_readi"),
                    writei: symbol!("snd_pcm_writei"),
                    recover: symbol!("snd_pcm_recover"),
                    drain: symbol!("snd_pcm_drain"),
                    _library: library,
                })
            }
        }

        /// Opens the `default` (plug) device for one direction and configures it
        /// as 8kHz mono S16LE. `snd_pcm_set_params` either installs exactly that
        /// — converting through the plug layer where the hardware needs it — or
        /// fails; there is no second-guessing a half-configured stream.
        fn open_stream(&self, stream: c_int) -> Result<Pcm, CallAudioError> {
            let name = CString::new("default").expect("a constant without interior NULs");
            let mut pcm: Pcm = std::ptr::null_mut();
            let what = if stream == STREAM_CAPTURE {
                "Microphone"
            } else {
                "Speaker"
            };
            let code = unsafe { (self.open)(&mut pcm, name.as_ptr(), stream, 0) };
            if code < 0 {
                return Err(unavailable(what, code));
            }
            // 8000Hz, 1 channel, S16_LE, interleaved, with soft resampling on:
            // the plug layer converts whatever the hardware actually runs.
            let code = unsafe {
                (self.set_params)(
                    pcm,
                    FORMAT_S16_LE,
                    ACCESS_RW_INTERLEAVED,
                    1,
                    super::CALL_SAMPLE_RATE,
                    1,
                    LATENCY_US,
                )
            };
            if code < 0 {
                unsafe { (self.close)(pcm) };
                return Err(unavailable(what, code));
            }
            Ok(pcm)
        }
    }

    /// Opens the default microphone and reads it on a dedicated thread.
    ///
    /// ALSA's `readi` blocks, and blocking the async runtime's thread is not
    /// ALSA's to do — so the blocking lives on this thread and the runtime sees
    /// only the channel.
    pub fn open_microphone() -> Result<Microphone, CallAudioError> {
        let alsa = Arc::new(Alsa::load()?);
        let pcm = alsa.open_stream(STREAM_CAPTURE)?;
        let (sender, receiver) = std_mpsc::channel::<Vec<i16>>();
        let reader = alsa.clone();
        let thread = std::thread::Builder::new()
            .name("migo-call-capture".to_owned())
            .spawn(move || {
                let mut buffer = [0i16; 160];
                loop {
                    let frames = unsafe {
                        (reader.readi)(pcm, buffer.as_mut_ptr().cast::<c_void>(), READ_FRAMES)
                    };
                    if frames < 0 {
                        // -EPIPE (overrun) and friends: recoverable, and
                        // recovering is one call. Anything else — the device
                        // unplugged, say — ends the thread, and the channel
                        // closing is the pump's cue that capture is over.
                        if unsafe { (reader.recover)(pcm, frames as c_int, 1) } < 0 {
                            break;
                        }
                        continue;
                    }
                    if frames > 0 && sender.send(buffer[..frames as usize].to_vec()).is_err() {
                        break; // the receiver is gone: the call tore down
                    }
                }
                unsafe { (reader.close)(pcm) };
            })
            .map_err(|error| unavailable("Microphone", error))?;
        Ok(Microphone {
            frames: receiver,
            rate: super::CALL_SAMPLE_RATE,
            _guard: super::MicrophoneGuard::Alsa(thread),
        })
    }

    /// Opens the default speaker on a dedicated writer thread.
    pub fn open_speaker() -> Result<Speaker, CallAudioError> {
        let alsa = Arc::new(Alsa::load()?);
        let pcm = alsa.open_stream(STREAM_PLAYBACK)?;
        let (sender, receiver) = std_mpsc::channel::<Vec<i16>>();
        let writer = alsa.clone();
        let thread = std::thread::Builder::new()
            .name("migo-call-playback".to_owned())
            .spawn(move || {
                while let Ok(chunk) = receiver.recv() {
                    // `writei` may consume a partial chunk; the rest is written
                    // until it is all out or the device is beyond saving.
                    let mut written = 0usize;
                    while written < chunk.len() {
                        let frames = unsafe {
                            (writer.writei)(
                                pcm,
                                chunk[written..].as_ptr().cast::<c_void>(),
                                (chunk.len() - written) as c_ulong,
                            )
                        };
                        if frames < 0 {
                            if unsafe { (writer.recover)(pcm, frames as c_int, 1) } < 0 {
                                // The speaker is gone; close and let the queue
                                // drain into the void.
                                unsafe { (writer.close)(pcm) };
                                return;
                            }
                            continue;
                        }
                        written += frames as usize;
                    }
                }
                // The sender is gone: the call tore down. Drain so the last
                // words are not cut off, then close.
                unsafe { (writer.drain)(pcm) };
                unsafe { (writer.close)(pcm) };
            })
            .map_err(|error| unavailable("Speaker", error))?;
        Ok(Speaker {
            frames: sender,
            rate: super::CALL_SAMPLE_RATE,
            _guard: super::SpeakerGuard::Alsa(thread),
        })
    }
}

// --- the cpal backend: the device's native configuration ---

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod cpal_backend {
    use super::{unavailable, CallAudioError, Microphone, Speaker};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{SampleFormat, Stream, StreamConfig};
    use std::collections::VecDeque;
    use std::sync::mpsc as std_mpsc;
    use std::sync::{Arc, Mutex};

    /// Opens the default microphone on its native configuration.
    ///
    /// WASAPI and CoreAudio refuse to build a stream at 8000Hz, so the stream
    /// runs at whatever the device actually does; the reported [`Microphone::rate`]
    /// is what the pump resamples from. Every callback mixes the device's
    /// channels down to mono — a call is mono, and the codec is defined on mono.
    pub fn open_microphone() -> Result<Microphone, CallAudioError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| unavailable("Microphone", "no input device"))?;
        let supported = device
            .default_input_config()
            .map_err(|error| unavailable("Microphone", error))?;
        let config = supported.config();
        let channels = config.channels as usize;
        let (sender, receiver) = std_mpsc::channel::<Vec<i16>>();
        let stream = match supported.sample_format() {
            SampleFormat::F32 => device
                .build_input_stream::<f32, _, _>(
                    &config,
                    move |data, _| {
                        let _ = sender.send(mix_f32(data, channels));
                    },
                    |_| {},
                    None,
                )
                .map_err(|error| unavailable("Microphone", error))?,
            SampleFormat::I16 => device
                .build_input_stream::<i16, _, _>(
                    &config,
                    move |data, _| {
                        let _ = sender.send(mix_i16(data, channels));
                    },
                    |_| {},
                    None,
                )
                .map_err(|error| unavailable("Microphone", error))?,
            SampleFormat::U16 => device
                .build_input_stream::<u16, _, _>(
                    &config,
                    move |data, _| {
                        // cpal's U16 has its origin at 32768, so the conversion
                        // is a sign-bit flip, not a scale.
                        let _ = sender.send(mix_u16(data, channels));
                    },
                    |_| {},
                    None,
                )
                .map_err(|error| unavailable("Microphone", error))?,
            other => {
                return Err(unavailable(
                    "Microphone",
                    format!("an unhandled sample format ({other:?})"),
                ))
            }
        };
        stream
            .play()
            .map_err(|error| unavailable("Microphone", error))?;
        Ok(Microphone {
            frames: receiver,
            rate: config.sample_rate.0,
            _guard: super::MicrophoneGuard::Cpal(stream),
        })
    }

    /// Opens the default speaker on its native configuration.
    ///
    /// The device's callback pulls from a queue the pump keeps fed; an empty
    /// queue plays silence, because a moment of nothing is what a speaker does
    /// when its writer is late — not an error, and not a reason to stop.
    pub fn open_speaker() -> Result<Speaker, CallAudioError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| unavailable("Speaker", "no output device"))?;
        let supported = device
            .default_output_config()
            .map_err(|error| unavailable("Speaker", error))?;
        let config = supported.config();
        let channels = config.channels as usize;
        let (sender, receiver) = std_mpsc::channel::<Vec<i16>>();
        let queue = Arc::new(Mutex::new(PlaybackQueue {
            pending: VecDeque::new(),
            receiver,
        }));
        let stream = match supported.sample_format() {
            SampleFormat::F32 => {
                build_output::<f32>(&device, &config, channels, queue.clone(), |sample| {
                    f32::from(sample) / 32_768.0
                })?
            }
            SampleFormat::I16 => {
                build_output::<i16>(&device, &config, channels, queue.clone(), |s| s)?
            }
            SampleFormat::U16 => build_output::<u16>(
                &device,
                &config,
                channels,
                queue.clone(),
                // The same origin flip, the other way: 0 becomes 32768.
                |sample| (sample as u16) ^ 0x8000,
            )?,
            other => {
                return Err(unavailable(
                    "Speaker",
                    format!("an unhandled sample format ({other:?})"),
                ))
            }
        };
        stream
            .play()
            .map_err(|error| unavailable("Speaker", error))?;
        Ok(Speaker {
            frames: sender,
            rate: config.sample_rate.0,
            _guard: super::SpeakerGuard::Cpal(stream),
        })
    }

    /// One format's output stream: pull mono samples from the queue, spread
    /// them across the device's channels, convert through `to_device`.
    fn build_output<T>(
        device: &cpal::Device,
        config: &StreamConfig,
        channels: usize,
        queue: Arc<Mutex<PlaybackQueue>>,
        to_device: fn(i16) -> T,
    ) -> Result<Stream, CallAudioError>
    where
        T: cpal::SizedSample,
    {
        device
            .build_output_stream::<T, _, _>(
                config,
                move |data, _| {
                    // A poisoned lock or a missed tick plays silence; an audio
                    // callback must not panic over a moment of nothing.
                    if let Ok(mut queue) = queue.lock() {
                        queue.fill(data, channels, to_device);
                    } else {
                        for sample in data.iter_mut() {
                            *sample = to_device(0);
                        }
                    }
                },
                |_| {},
                None,
            )
            .map_err(|error| unavailable("Speaker", error))
    }

    /// The speaker's shared queue: what the pump sent and the callback has not
    /// played yet.
    struct PlaybackQueue {
        pending: VecDeque<i16>,
        receiver: std_mpsc::Receiver<Vec<i16>>,
    }

    impl PlaybackQueue {
        /// Fills one output buffer: refill from the channel when the pending
        /// samples run dry, spread each mono sample across every channel, and
        /// fall quiet rather than stall if nothing is left.
        fn fill<T>(&mut self, data: &mut [T], channels: usize, to_device: fn(i16) -> T) {
            for frame in data.chunks_mut(channels) {
                if self.pending.is_empty() {
                    match self.receiver.try_recv() {
                        Ok(chunk) => self.pending.extend(chunk),
                        Err(std_mpsc::TryRecvError::Disconnected) => {} // the call tore down
                        Err(std_mpsc::TryRecvError::Empty) => {}        // the pump is late
                    }
                }
                let sample = self.pending.pop_front().unwrap_or(0);
                for slot in frame.iter_mut() {
                    *slot = to_device(sample);
                }
            }
        }
    }

    /// Mixes interleaved f32 frames down to mono i16.
    fn mix_f32(data: &[f32], channels: usize) -> Vec<i16> {
        data.chunks(channels)
            .map(|frame| {
                let sum: f32 = frame.iter().sum();
                (sum / frame.len() as f32).clamp(-1.0, 1.0)
            })
            .map(|unit| (unit * 32_767.0).round() as i16)
            .collect()
    }

    /// Mixes interleaved i16 frames down to mono.
    fn mix_i16(data: &[i16], channels: usize) -> Vec<i16> {
        data.chunks(channels)
            .map(|frame| {
                let sum: i32 = frame.iter().map(|&s| i32::from(s)).sum();
                (sum / frame.len() as i32) as i16
            })
            .collect()
    }

    /// Mixes interleaved u16 frames (origin 32768) down to mono i16.
    fn mix_u16(data: &[u16], channels: usize) -> Vec<i16> {
        data.chunks(channels)
            .map(|frame| {
                let sum: i32 = frame.iter().map(|&s| i32::from(s) - 32_768).sum::<i32>();
                (sum / frame.len() as i32) as i16
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// µ-law is not lossless — it is 8 bits of logarithmic quantization — but
    /// its error is bounded, and the bound is the codec's own: half a segment
    /// step, worst case in the loudest segment.
    #[test]
    fn a_linear_sample_round_trips_within_the_codecs_own_error() {
        assert_eq!(ulaw_to_linear(linear_to_ulaw(0)), 0, "silence is exact");
        for sample in [
            1i16, -1, 100, -100, 1_000, -1_000, 5_000, -5_000, 10_000, -10_000, 20_000, -20_000,
            32_635, -32_635, 32_767, -32_768,
        ] {
            let decoded = ulaw_to_linear(linear_to_ulaw(sample));
            assert!(
                (decoded - sample).abs() <= 256,
                "{sample} came back as {decoded}"
            );
        }
    }

    #[test]
    fn a_pair_of_samples_of_opposite_sign_differ_only_in_the_sign_bit() {
        for sample in [1i16, 100, 1_000, 10_000, 32_767] {
            let positive = linear_to_ulaw(sample);
            let negative = linear_to_ulaw(-sample);
            assert_eq!(positive ^ negative, 0x80, "for {sample}");
        }
    }

    #[test]
    fn the_loudest_samples_clip_rather_than_wrap() {
        let loud = ulaw_to_linear(linear_to_ulaw(32_767));
        assert!(
            loud > 31_000,
            "the positive rail decodes loud, not wrapped: {loud}"
        );
        let quiet = ulaw_to_linear(linear_to_ulaw(-32_768));
        assert!(
            quiet < -31_000,
            "the negative rail decodes loud, not wrapped: {quiet}"
        );
    }

    #[test]
    fn a_frame_round_trips_as_a_run_of_samples() {
        let samples: Vec<i16> = (0..160)
            .map(|i| ((i * 37) % 4_001 - 2_000) as i16)
            .collect();
        let encoded = ulaw_encode(&samples);
        assert_eq!(encoded.len(), 160, "one byte per sample, no framing");
        let decoded = ulaw_decode(&encoded);
        for (original, back) in samples.iter().zip(decoded.iter()) {
            assert!((original - back).abs() <= 256);
        }
    }

    #[test]
    fn decimation_picks_every_other_sample_exactly() {
        let mut resampler = Resampler::new(2, 1);
        let mut out = Vec::new();
        resampler.process(&[0, 100, 200, 300], &mut out);
        resampler.process(&[400, 500, 600, 700], &mut out);
        assert_eq!(out, vec![0, 200, 400, 600]);
    }

    #[test]
    fn doubling_interpolates_the_halfway_points_and_chunks_join_smoothly() {
        let mut resampler = Resampler::new(1, 2);
        let mut out = Vec::new();
        resampler.process(&[0, 100], &mut out);
        assert_eq!(
            out,
            vec![0, 50],
            "the last interval waits for its right neighbour"
        );
        resampler.process(&[200, 300], &mut out);
        assert_eq!(
            out,
            vec![0, 50, 100, 150, 200, 250],
            "the carried state finishes the interpolation across the chunk join"
        );
    }

    #[test]
    fn a_second_of_audio_comes_out_roughly_a_second_of_audio() {
        let mut up = Resampler::new(8_000, 48_000);
        let mut out = Vec::new();
        // 40 chunks of 200 samples = 8_000 samples = one second at 8kHz.
        for chunk in 0..40 {
            let input: Vec<i16> = (0..200).map(|i| i16::from((i + chunk) % 100)).collect();
            up.process(&input, &mut out);
        }
        assert!(
            (47_900..=48_100).contains(&out.len()),
            "one second at 48kHz, give or take the carried interval: {}",
            out.len()
        );

        let mut down = Resampler::new(48_000, 8_000);
        let mut out = Vec::new();
        for chunk in 0..40 {
            let input: Vec<i16> = (0..200).map(|i| i16::from((i + chunk) % 100)).collect();
            down.process(&input, &mut out);
        }
        // 40 chunks of 200 = 8_000 input samples = one sixth of a second at 48kHz.
        assert!(
            (1_290..=1_360).contains(&out.len()),
            "one sixth of a second at 8kHz: {}",
            out.len()
        );
    }

    #[test]
    fn an_empty_chunk_carries_state_without_inventing_samples() {
        let mut resampler = Resampler::new(1, 2);
        let mut out = Vec::new();
        resampler.process(&[100, 200], &mut out);
        let before = out.len();
        resampler.process(&[], &mut out);
        assert_eq!(out.len(), before, "nothing in, nothing out");
        resampler.process(&[300, 400], &mut out);
        assert_eq!(
            out,
            vec![100, 150, 200, 250, 300, 350],
            "the state survived the silence"
        );
    }
}
