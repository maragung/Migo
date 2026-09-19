//! Which microphone and which speaker a call opens.
//!
//! The phone answers this with its route picker and the web with `call-devices.ts`; the
//! desktop's answer is a settings pane, because a desktop's sound devices change far less often
//! than a phone's and a call is not the moment to go hunting through a menu for a headset that
//! was plugged in last week. What is enumerated here is what those two clients enumerate: what
//! this machine can actually carry a call on, never a fixed list, and never a device the
//! platform would refuse to open.
//!
//! # Two spellings, one meaning
//!
//! A [`CallDevice::id`] is a handle the audio layer can open and nothing else — on Linux an ALSA
//! PCM name, on Windows and macOS a cpal device name. It is deliberately opaque to the settings
//! pane, which draws [`CallDevice::label`] and stores the id for the audio layer to read back: a
//! name is what a person recognises, and a handle is what survives the round trip. The ids are
//! each backend's own, because the two backends open devices in two different ways and an
//! abstraction over "how a sound card is named" would be a third naming scheme nobody speaks.
//!
//! # The empty list is an answer
//!
//! A machine with no sound card, a container with no `/proc/asound`, a Linux box whose ALSA
//! library is missing: all three enumerate nothing, and all three can still make a call —
//! through the system's own default, which is the row the settings pane draws above whatever
//! this module returns. An empty list therefore means "nothing to choose between", never "no
//! calls", and the pane says so in those words.
//!
//! # A stale id is not an error
//!
//! A remembered device can be gone by the time a call opens — unplugged, renumbered by the
//! kernel, or a name carried over from another machine's settings file. Nothing here treats
//! that as a failure: the pane compares the remembered id against the current list and says the
//! device is not connected, and the audio layer opens the system default instead. The choice
//! itself is left in the settings record rather than rewritten, because a headset unplugged for
//! an afternoon is a headset the person wants back when it is plugged in again.

/// One device a call can be carried by: the handle the audio layer opens, and the name a person
/// picks it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallDevice {
    /// The platform's handle for this device, as [`crate::net::call_audio`] will open it. Opaque
    /// to everything that is not the audio layer.
    pub id: String,
    /// What the settings pane draws: the device's own name, as the platform reports it.
    pub label: String,
}

/// Which way a device has to carry audio for a call to have a use for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Input,
    Output,
}

/// Every microphone this machine can record a call from, in the platform's own order.
#[must_use]
pub fn microphones() -> Vec<CallDevice> {
    platform::devices(Direction::Input)
}

/// Every speaker this machine can play a call through, in the platform's own order.
#[must_use]
pub fn speakers() -> Vec<CallDevice> {
    platform::devices(Direction::Output)
}

/// Linux enumerates through `/proc/asound`, not through libasound.
///
/// Those two files are the kernel's own account of the cards it has, and reading them costs a
/// `read` where asking ALSA costs a `dlopen` — and the pane wants an answer on the frame it is
/// opened, on a machine that may have no sound hardware at all. What the files give is exactly
/// what a picker needs: which cards exist, what each is called, and which of their PCM devices
/// carry playback and which carry capture, because a card that can only play audio has no
/// business in the microphone list.
#[cfg(target_os = "linux")]
mod platform {
    use super::{CallDevice, Direction};
    use std::collections::BTreeMap;

    /// The kernel's list of cards, and of the PCM devices on them.
    const CARDS: &str = "/proc/asound/cards";
    const PCMS: &str = "/proc/asound/pcm";

    pub(super) fn devices(direction: Direction) -> Vec<CallDevice> {
        let Ok(pcms) = std::fs::read_to_string(PCMS) else {
            return Vec::new();
        };
        let cards = std::fs::read_to_string(CARDS)
            .map(|text| parse_cards(&text))
            .unwrap_or_default();
        parse_pcms(&pcms)
            .into_iter()
            .filter(|pcm| match direction {
                Direction::Input => pcm.capture > 0,
                Direction::Output => pcm.playback > 0,
            })
            .map(|pcm| CallDevice {
                // `plughw` rather than `hw`: a call runs at 8kHz mono, and the plug layer is what
                // converts that to whatever the hardware actually does. The card is named by its
                // id rather than its number, because the kernel reassigns numbers between boots
                // — a saved choice that survives a reboot is the whole point of saving it — and
                // the number is only the fallback for a card the kernel named nothing.
                id: match cards.get(&pcm.card).map(|card| card.id.as_str()) {
                    Some(id) if !id.is_empty() => format!("plughw:CARD={id},DEV={}", pcm.device),
                    _ => format!("plughw:{},{}", pcm.card, pcm.device),
                },
                label: match cards.get(&pcm.card).map(|card| card.label.as_str()) {
                    Some(card) if !card.label.is_empty() => format!("{card} \u{2014} {}", pcm.name),
                    _ => pcm.name.clone(),
                },
            })
            .collect()
    }

    /// One card, as `/proc/asound/cards` describes it: the id ALSA's own tools print in
    /// brackets, and the long name a person recognises.
    struct Card {
        id: String,
        label: String,
    }

    /// One PCM device, as `/proc/asound/pcm` describes it.
    struct Pcm {
        card: u32,
        device: u32,
        name: String,
        playback: u32,
        capture: u32,
    }

    /// Reads the card table.
    ///
    /// The shape is `<number> [<id>]: <driver> - <long name>` — ` 0 [PCH ]: HDA-Intel - HDA Intel
    /// PCH` — with the driver's own description continuing on the indented lines below. The
    /// bracket is what separates a card from its continuation lines: only a card line has one,
    /// and the number before it parses as a card only there, so neither test is needed on its
    /// own. The long name is what is kept, because "HDA Intel PCH" tells a person which sockets
    /// to look at and "HDA-Intel" only says which driver claimed them.
    fn parse_cards(text: &str) -> BTreeMap<u32, Card> {
        let mut cards = BTreeMap::new();
        for line in text.lines() {
            let Some((number, rest)) = line.split_once('[') else {
                continue;
            };
            let Ok(number) = number.trim().parse::<u32>() else {
                continue;
            };
            let Some((id, rest)) = rest.split_once(']') else {
                continue;
            };
            let described = rest.trim_start_matches(':').trim();
            let long = described
                .split_once(" - ")
                .map_or(described, |(_, long)| long)
                .trim();
            cards.insert(
                number,
                Card {
                    id: id.trim().to_owned(),
                    label: long.to_owned(),
                },
            );
        }
        cards
    }

    /// Reads the PCM table: `<card>-<device>: <name> : playback <n> : capture <n>`, where each
    /// direction clause is optional — a card that only plays audio carries only the first. The
    /// number in a clause is how many streams the device takes in that direction, so the filter
    /// upstream asks for more than zero rather than for the clause's presence.
    fn parse_pcms(text: &str) -> Vec<Pcm> {
        text.lines().filter_map(parse_pcm).collect()
    }

    /// One line of the PCM table. A line that is not one — a blank, a header, a shape a future
    /// kernel writes differently — is not an error worth reporting: it is a device that does not
    /// appear in a picker, which the system-default row still covers.
    fn parse_pcm(line: &str) -> Option<Pcm> {
        let mut clauses = line.split(" : ");
        let (address, name) = clauses.next()?.split_once(": ")?;
        let (card, device) = address.trim().split_once('-')?;
        let mut pcm = Pcm {
            card: card.trim().parse().ok()?,
            device: device.trim().parse().ok()?,
            name: name.trim().to_owned(),
            playback: 0,
            capture: 0,
        };
        for clause in clauses {
            let clause = clause.trim();
            if let Some(count) = clause.strip_prefix("playback ") {
                pcm.playback = count.trim().parse().unwrap_or(0);
            } else if let Some(count) = clause.strip_prefix("capture ") {
                pcm.capture = count.trim().parse().unwrap_or(0);
            }
        }
        Some(pcm)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Two cards with the shape the kernel actually writes: the number right-aligned in a
        /// two-wide field, then the id in brackets, then the driver and its long name, with the
        /// card's bus address on the line below indented under it.
        const CARDS_TEXT: &str = concat!(
            " 0 [PCH            ]: HDA-Intel - HDA Intel PCH\n",
            "                      HDA Intel PCH at 0xf7f10000 irq 126\n",
            " 1 [Device         ]: USB-Audio - USB Device 0x46d:0x825\n",
            "                      USB Device 0x46d:0x825 at usb-0000:00:14.0-1, high speed\n",
        );

        /// Four devices on two cards, covering all three shapes a line can have: both
        /// directions, capture only, and playback only.
        const PCMS_TEXT: &str = concat!(
            "00-00: ALC887-VD Analog : playback 1 : capture 1\n",
            "00-02: ALC887-VD Alt Analog : capture 1\n",
            "00-03: ALC887-VD Digital : playback 1\n",
            "01-00: USB Audio : playback 1 : capture 1\n",
        );

        #[test]
        fn the_card_table_keeps_the_long_name_and_the_bracketed_id() {
            let cards = parse_cards(CARDS_TEXT);
            assert_eq!(cards.len(), 2);
            assert_eq!(cards[&0].id, "PCH");
            assert_eq!(cards[&0].label, "HDA Intel PCH");
            assert_eq!(cards[&1].id, "Device");
            assert_eq!(cards[&1].label, "USB Device 0x46d:0x825");
        }

        /// The address line under a card is not a card: it has no bracket, so it cannot be
        /// mistaken for one however much it looks like a device name.
        #[test]
        fn a_cards_address_line_is_not_a_card() {
            let text = " 0 [PCH]: HDA-Intel - HDA Intel PCH\n  HDA Intel PCH at 0xf7f10000\n";
            assert_eq!(parse_cards(text).len(), 1);
        }

        #[test]
        fn the_pcm_table_reads_both_directions_and_the_ones_that_only_have_one() {
            let pcms = parse_pcms(PCMS_TEXT);
            assert_eq!(pcms.len(), 4);
            assert_eq!((pcms[0].card, pcms[0].device), (0, 0));
            assert_eq!(pcms[0].name, "ALC887-VD Analog");
            assert_eq!((pcms[0].playback, pcms[0].capture), (1, 1));
            assert_eq!((pcms[1].playback, pcms[1].capture), (0, 1));
            assert_eq!((pcms[2].playback, pcms[2].capture), (1, 0));
            assert_eq!((pcms[3].card, pcms[3].device), (1, 0));
            assert_eq!(pcms[3].name, "USB Audio");
        }

        /// The picker's two lists come from one table, divided by the direction each device can
        /// carry: a card that only plays audio must not be offered as a microphone, and the
        /// device that only records must not be offered as a speaker.
        #[test]
        fn each_direction_gets_only_the_devices_that_carry_it() {
            let pcms = parse_pcms(PCMS_TEXT);
            let inputs: Vec<&str> = pcms
                .iter()
                .filter(|pcm| pcm.capture > 0)
                .map(|pcm| pcm.name.as_str())
                .collect();
            let outputs: Vec<&str> = pcms
                .iter()
                .filter(|pcm| pcm.playback > 0)
                .map(|pcm| pcm.name.as_str())
                .collect();
            assert_eq!(
                inputs,
                ["ALC887-VD Analog", "ALC887-VD Alt Analog", "USB Audio"]
            );
            assert_eq!(
                outputs,
                ["ALC887-VD Analog", "ALC887-VD Digital", "USB Audio"]
            );
        }

        /// A line the kernel does not write this way is skipped rather than guessed at: the
        /// picker loses a row, and the system default is still there to be used.
        #[test]
        fn a_line_that_is_not_a_pcm_is_skipped() {
            assert!(parse_pcms("\n\nnot a pcm line\n00-00: broken\n").is_empty());
        }
    }
}

/// Windows and macOS enumerate through cpal, which already knows how to ask each host for its
/// own device list — WASAPI's endpoints, CoreAudio's devices — and already owns the names the
/// audio layer opens them by, so the two halves of the round trip are the same string by
/// construction and no mapping table can drift.
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod platform {
    use super::{CallDevice, Direction};
    use cpal::traits::{DeviceTrait, HostTrait};

    pub(super) fn devices(direction: Direction) -> Vec<CallDevice> {
        let host = cpal::default_host();
        // The two directions are two iterator types, so they are collected in their own arms
        // rather than unified by a trait object: nothing downstream needs them to be one type.
        let devices: Vec<cpal::Device> = match direction {
            Direction::Input => host
                .input_devices()
                .map(|devices| devices.collect())
                .unwrap_or_default(),
            Direction::Output => host
                .output_devices()
                .map(|devices| devices.collect())
                .unwrap_or_default(),
        };
        devices
            // A device the host will not name is a device the audio layer cannot be asked to
            // open again, so it is left out rather than drawn as a row that could never work.
            .iter()
            .filter_map(|device| device.name().ok())
            .filter(|name| !name.is_empty())
            .map(|name| CallDevice {
                id: name.clone(),
                label: name,
            })
            .collect()
    }
}
