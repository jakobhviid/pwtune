//! Speaker classes: the envelope a design is allowed to work inside.
//!
//! Correcting a response means asking a driver for more output at some frequency, and
//! every driver has a floor below which it moves air nowhere and only heats up.
//!
//! No magnitude sweep can find that floor, however well the microphone is calibrated,
//! because the floor is a *mechanical* limit: excursion, thermal capacity, and the
//! trip point of whatever protection sits in front of the driver. A driver's −20 dB
//! point is not its safe-operating point, and a small-signal response says nothing
//! about either. So the floor is *declared* here instead, from something the owner can
//! see: the size and kind of the speaker. (Measuring it needs a different experiment —
//! a two-level sweep, or distortion against frequency — not a better microphone.)
//!
//! These corners are conservative starting envelopes, not measurements. The corner
//! is shown next to every class so it reads as the estimate it is, and
//! `--highpass` overrides it outright for anyone who knows better — a datasheet, a
//! vendor tuning file, or their own ears.
use crate::dsp::Limits;

pub struct Class {
    pub name: &'static str,
    /// Where the driver stops being able to help, or `None` to declare no floor.
    pub highpass: Option<f64>,
    /// What the owner looks at to recognise it.
    pub look_for: &'static str,
}

/// Ordered small to large. `fullrange` is last and also covers any system with a
/// subwoofer, where a high-pass at the sink would remove the sub's whole job.
pub const CLASSES: &[Class] = &[
    Class {
        name: "laptop",
        highpass: Some(150.0),
        look_for: "ultrabook or tablet, no separate woofer grilles",
    },
    Class {
        name: "laptop-woofer",
        highpass: Some(100.0),
        look_for: "larger chassis with dedicated woofers or quad speakers",
    },
    Class {
        name: "desktop",
        highpass: Some(80.0),
        look_for: "small powered speakers or a soundbar, 2-3 inch driver",
    },
    Class {
        name: "bookshelf",
        highpass: Some(50.0),
        look_for: "5 inch woofer or larger",
    },
    Class {
        name: "fullrange",
        highpass: None,
        look_for: "floorstanders, studio monitors, or any system with a subwoofer",
    },
];

/// The class that declares no floor — what a caller naming nothing gets, because
/// under-serving a small speaker is recoverable and quietly high-passing a system
/// that reaches low is not.
pub fn unbounded() -> &'static Class {
    CLASSES.last().expect("CLASSES is never empty")
}

/// Lowest band worth placing when nobody has declared anything at all.
///
/// Naming `fullrange` asserts the speaker reaches low, and that assertion is what
/// makes a boost down there safe. A run that declared nothing has made no such
/// assertion, so it gets no filter *and* no boost in the region a small driver
/// cannot use — which is what "under-serving is recoverable" is supposed to mean.
pub const UNDECLARED_FLOOR: f64 = 120.0;

/// Bounds for a run where nothing was declared.
pub fn undeclared_limits() -> Limits {
    Limits { correct_from: UNDECLARED_FLOOR, ..Limits::default() }
}

pub fn by_name(name: &str) -> Option<&'static Class> {
    CLASSES.iter().find(|c| c.name.eq_ignore_ascii_case(name))
}

pub fn names() -> Vec<&'static str> {
    CLASSES.iter().map(|c| c.name).collect()
}

impl Class {
    /// How this class reads in a menu or a profile header.
    pub fn corner_label(&self) -> String {
        match self.highpass {
            Some(f) => format!("high-pass {f:.0} Hz"),
            None => "no high-pass".to_string(),
        }
    }

    /// The design bounds this class implies.
    ///
    /// Correction stops half an octave above the high-pass corner, not at it. A band
    /// sitting on the corner undoes the filter outright — a +8 dB band at an 80 Hz
    /// corner takes the response there from -3.0 dB to +5.0 dB and raises
    /// pink-weighted 20-160 Hz energy by 5.3 dB, so the filter ends up worse than
    /// absent. At 1.41x the corner a 4th-order response is within 0.3 dB of unity,
    /// so a band there cannot meaningfully fight it.
    pub fn limits(&self, highpass: Option<f64>) -> Limits {
        let floor = match highpass {
            Some(f) => f * 2f64.powf(0.5),
            None => 0.0,
        };
        Limits { correct_from: floor, ..Limits::default() }
    }
}

/// SMBIOS chassis types that mean "this machine is carried around", and therefore
/// that its internal speakers are small ones in a sealed cavity.
const PORTABLE_CHASSIS: &[&str] = &["8", "9", "10", "11", "14", "30", "31", "32"];

/// Whether this machine is a portable, from DMI. World-readable, no privileges, and
/// present on any firmware that reports SMBIOS; absent on some VMs and ARM boards,
/// where the answer is "assume not".
fn chassis_is_portable() -> bool {
    std::fs::read_to_string("/sys/class/dmi/id/chassis_type")
        .map(|v| PORTABLE_CHASSIS.contains(&v.trim()))
        .unwrap_or(false)
}

/// A guess at the family, for use as a menu default only.
///
/// Two things have to agree: the machine is a portable, and the sink is its built-in
/// codec rather than something plugged into it. A sink name alone cannot carry this —
/// `analog`, `hda` and `pch` appear just as often on a desktop tower feeding
/// bookshelf speakers, and defaulting *those* to a 150 Hz high-pass would quietly
/// remove output the speakers can reproduce. Anything short of both signals declares
/// no floor and leaves the choice to whoever can see the speaker.
pub fn detect(name: &str, desc: &str, nick: &str) -> &'static Class {
    detect_from(chassis_is_portable(), name, desc, nick)
}

/// The decision, with the machine fact passed in so it can be tested.
pub fn detect_from(portable: bool, name: &str, desc: &str, nick: &str) -> &'static Class {
    if !portable {
        return unbounded();
    }
    let blob = format!("{name} {desc} {nick}").to_lowercase();
    let plugged_in = ["hdmi", "displayport", "usb", "bluez", "a2dp", "iec958", "thunderbolt"];
    if plugged_in.iter().any(|k| blob.contains(k)) {
        return unbounded();
    }
    let built_in = ["sof", "hda", "analog", "pch", "speaker", "acp"];
    if built_in.iter().any(|k| blob.contains(k)) {
        return by_name("laptop").unwrap_or_else(|| unbounded());
    }
    unbounded()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_is_reachable_by_its_own_name() {
        for c in CLASSES {
            assert!(by_name(c.name).is_some(), "{} not found", c.name);
        }
        assert!(by_name("LAPTOP").is_some(), "names are case-insensitive");
        assert!(by_name("nonsense").is_none());
    }

    #[test]
    fn declaring_nothing_declares_no_floor() {
        assert!(unbounded().highpass.is_none());
        assert_eq!(unbounded().limits(None).correct_from, 0.0);
    }

    #[test]
    fn corners_get_smaller_as_speakers_get_bigger() {
        let corners: Vec<f64> =
            CLASSES.iter().filter_map(|c| c.highpass).collect();
        assert!(corners.windows(2).all(|w| w[0] > w[1]), "{corners:?} should descend");
    }

    /// Half an octave of clearance above the corner, so no band can undo the filter.
    #[test]
    fn correction_stops_above_the_high_pass_corner() {
        for c in CLASSES.iter().filter(|c| c.highpass.is_some()) {
            let corner = c.highpass.unwrap();
            let floor = c.limits(c.highpass).correct_from;
            assert!(floor > corner, "{}: floor {floor} must clear corner {corner}", c.name);
            assert!((floor / corner - 2f64.sqrt()).abs() < 1e-9, "{}: half an octave", c.name);
        }
    }

    /// Declaring nothing is not the same as declaring fullrange.
    #[test]
    fn an_undeclared_run_still_refuses_the_lowest_bands() {
        assert_eq!(unbounded().limits(None).correct_from, 0.0);
        assert!(undeclared_limits().correct_from >= 120.0);
    }

    const LAPTOP_SINK: &str =
        "alsa_output.pci-0000_00_1f.3-platform-sof_sdw.HiFi__Speaker__sink";

    #[test]
    fn a_portables_built_in_codec_guesses_the_smaller_laptop_tier() {
        assert_eq!(detect_from(true, LAPTOP_SINK, "Speaker", "").name, "laptop");
    }

    /// A display or dock sink is not a built-in speaker, and a guess there would be
    /// a high-pass nobody asked for on equipment that may reach far lower.
    #[test]
    fn plugged_in_sinks_are_left_unbounded() {
        for n in [
            "alsa_output.pci-0000_01_00.1.hdmi-stereo",
            "alsa_output.usb-Focusrite",
            "bluez_output.AC_12",
        ] {
            assert!(detect_from(true, n, "", "").highpass.is_none(), "{n} should stay unbounded");
        }
    }

    /// The case a sink name cannot distinguish: a desktop's onboard analog output is
    /// named like a laptop's, and its speakers are usually the bigger ones. Guessing
    /// `laptop` there would pre-select a 150 Hz high-pass on a box that reaches lower.
    #[test]
    fn a_desktop_is_never_guessed_as_a_laptop() {
        for n in [
            "alsa_output.pci-0000_00_1f.3.analog-stereo",
            "alsa_output.pci-0000_00_1b.0.hda-analog",
            LAPTOP_SINK,
        ] {
            assert!(
                detect_from(false, n, "Built-in Audio", "").highpass.is_none(),
                "{n} on a non-portable chassis should stay unbounded"
            );
        }
    }

    /// No DMI to read (VMs, some ARM boards) means no claim either way.
    #[test]
    fn an_unknown_chassis_declares_no_floor() {
        assert!(detect_from(false, LAPTOP_SINK, "", "").highpass.is_none());
    }
}
