//! Wipe scheme registry: every major published overwrite standard.
//!
//! A scheme is an ordered list of passes; a pass either fills the disk with a
//! repeating byte pattern or with a pseudo-random stream. Verification policy
//! is configurable per session (none / last pass / every pass).
//!
//! NOTE ON SSDs: overwrite schemes were designed for magnetic media. Flash
//! translation layers (wear leveling, over-provisioning) mean an overwrite
//! cannot reach every physical cell on an SSD. DBAN surfaces this advisory
//! in the UI whenever a non-rotational disk is selected; NIST SP 800-88
//! recommends crypto-erase / SANITIZE commands for flash purge.

use serde::{Deserialize, Serialize};

/// What a single pass writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassKind {
    /// Repeating byte pattern (1..=3 bytes in every published standard).
    Fill(&'static [u8]),
    /// Pseudo-random stream, freshly seeded per pass from OS entropy.
    Random,
}

/// A single overwrite pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pass {
    /// What this pass writes.
    pub kind: PassKind,
}

impl Pass {
    /// A pass that writes a repeating byte `pattern`.
    pub const fn fill(pattern: &'static [u8]) -> Self {
        Pass {
            kind: PassKind::Fill(pattern),
        }
    }
    /// A pass that writes a fresh pseudo-random stream.
    pub const fn random() -> Self {
        Pass {
            kind: PassKind::Random,
        }
    }
    /// A pass of all-zero bytes.
    pub const fn zeros() -> Self {
        Pass::fill(&[0x00])
    }
    /// A pass of all-one bytes (`0xFF`).
    pub const fn ones() -> Self {
        Pass::fill(&[0xFF])
    }

    /// Short human label, e.g. "zeros (0x00)", "random", "0x92 49 24".
    pub fn label(&self) -> String {
        match self.kind {
            PassKind::Random => "random".to_string(),
            PassKind::Fill(p) => match p {
                [0x00] => "zeros (0x00)".to_string(),
                [0xFF] => "ones (0xFF)".to_string(),
                _ => {
                    let hex: Vec<String> = p.iter().map(|b| format!("{b:02X}")).collect();
                    format!("0x{}", hex.join(" "))
                }
            },
        }
    }
}

/// Verification policy for a wipe session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerifyMode {
    /// No read-back. Fastest; not recommended.
    None,
    /// Read back and verify the final pass only (the classic "Verify Last Pass").
    LastPass,
    /// Read back and verify every pass. Doubles total I/O.
    AllPasses,
}

impl VerifyMode {
    /// Short display label, e.g. `"last pass"`.
    pub fn label(&self) -> &'static str {
        match self {
            VerifyMode::None => "off",
            VerifyMode::LastPass => "last pass",
            VerifyMode::AllPasses => "all passes",
        }
    }
    /// Advance to the next mode in the `off → last → all → off` cycle.
    pub fn cycle(&self) -> VerifyMode {
        match self {
            VerifyMode::None => VerifyMode::LastPass,
            VerifyMode::LastPass => VerifyMode::AllPasses,
            VerifyMode::AllPasses => VerifyMode::None,
        }
    }
}

/// A named, documented wipe standard.
#[derive(Clone, Debug)]
pub struct Scheme {
    /// Stable machine id, used in reports.
    pub id: &'static str,
    /// Human-readable scheme name.
    pub name: &'static str,
    /// Where the standard comes from.
    pub origin: &'static str,
    /// One-paragraph description shown in the UI.
    pub description: &'static str,
    /// The ordered list of overwrite passes.
    pub passes: Vec<Pass>,
    /// Verification policy applied by default when this scheme is selected.
    pub default_verify: VerifyMode,
    /// Highlighted as the sane modern default in the UI.
    pub recommended: bool,
}

impl Scheme {
    /// Number of overwrite passes in the scheme.
    pub fn pass_count(&self) -> usize {
        self.passes.len()
    }
}

// Gutmann fixed patterns, passes 5..=31 of the 35-pass scheme
// (Gutmann, "Secure Deletion of Data from Magnetic and Solid-State Memory", 1996).
const GUTMANN_FIXED: [&[u8]; 27] = [
    &[0x55],
    &[0xAA],
    &[0x92, 0x49, 0x24],
    &[0x49, 0x24, 0x92],
    &[0x24, 0x92, 0x49],
    &[0x00],
    &[0x11],
    &[0x22],
    &[0x33],
    &[0x44],
    &[0x55],
    &[0x66],
    &[0x77],
    &[0x88],
    &[0x99],
    &[0xAA],
    &[0xBB],
    &[0xCC],
    &[0xDD],
    &[0xEE],
    &[0xFF],
    &[0x92, 0x49, 0x24],
    &[0x49, 0x24, 0x92],
    &[0x24, 0x92, 0x49],
    &[0x6D, 0xB6, 0xDB],
    &[0xB6, 0xDB, 0x6D],
    &[0xDB, 0x6D, 0xB6],
];

fn gutmann_passes() -> Vec<Pass> {
    let mut passes = Vec::with_capacity(35);
    for _ in 0..4 {
        passes.push(Pass::random());
    }
    for p in GUTMANN_FIXED {
        passes.push(Pass::fill(p));
    }
    for _ in 0..4 {
        passes.push(Pass::random());
    }
    passes
}

/// The full scheme registry, in the order presented by the UI.
pub fn all_schemes() -> Vec<Scheme> {
    vec![
        Scheme {
            id: "nist-clear",
            name: "NIST 800-88 Clear",
            origin: "NIST SP 800-88 Rev. 1 (2014)",
            description: "Single overwrite with zeros, verified. The modern industry \
                          baseline: one verified pass defeats any software-level recovery \
                          on magnetic and most flash media.",
            passes: vec![Pass::zeros()],
            default_verify: VerifyMode::LastPass,
            recommended: true,
        },
        Scheme {
            id: "prng",
            name: "PRNG Stream",
            origin: "classic boot-and-nuke",
            description: "One pass of OS-seeded pseudorandom data, verified by \
                          regenerating the stream. Raise 'rounds' for extra passes.",
            passes: vec![Pass::random()],
            default_verify: VerifyMode::LastPass,
            recommended: false,
        },
        Scheme {
            id: "dod-short",
            name: "DoD 5220.22-M (E)",
            origin: "US DoD NISPOM, 3-pass short variant",
            description: "Zeros, ones, then random, with final verification. The \
                          classic three-pass clearing procedure.",
            passes: vec![Pass::zeros(), Pass::ones(), Pass::random()],
            default_verify: VerifyMode::LastPass,
            recommended: false,
        },
        Scheme {
            id: "dod-ece",
            name: "DoD 5220.22-M (ECE)",
            origin: "US DoD NISPOM, 7-pass variant",
            description: "Seven passes: the E sequence, a random pass, then the E \
                          sequence again. Final pass verified.",
            passes: vec![
                Pass::zeros(),
                Pass::ones(),
                Pass::random(),
                Pass::random(),
                Pass::zeros(),
                Pass::ones(),
                Pass::random(),
            ],
            default_verify: VerifyMode::LastPass,
            recommended: false,
        },
        Scheme {
            id: "schneier",
            name: "Schneier 7-pass",
            origin: "B. Schneier, Applied Cryptography (1996)",
            description: "Ones, zeros, then five passes of random data.",
            passes: vec![
                Pass::ones(),
                Pass::zeros(),
                Pass::random(),
                Pass::random(),
                Pass::random(),
                Pass::random(),
                Pass::random(),
            ],
            default_verify: VerifyMode::LastPass,
            recommended: false,
        },
        Scheme {
            id: "gutmann",
            name: "Gutmann 35-pass",
            origin: "P. Gutmann (1996)",
            description: "The famous 35-pass scheme targeting 1990s encoding \
                          schemes (MFM/RLL). Overkill on modern drives; included \
                          for completeness and compliance checklists.",
            passes: gutmann_passes(),
            default_verify: VerifyMode::LastPass,
            recommended: false,
        },
        Scheme {
            id: "vsitr",
            name: "VSITR",
            origin: "German BSI, 7-pass",
            description: "Alternating zeros and ones for six passes, finishing \
                          with 0xAA.",
            passes: vec![
                Pass::zeros(),
                Pass::ones(),
                Pass::zeros(),
                Pass::ones(),
                Pass::zeros(),
                Pass::ones(),
                Pass::fill(&[0xAA]),
            ],
            default_verify: VerifyMode::LastPass,
            recommended: false,
        },
        Scheme {
            id: "rcmp-tssit",
            name: "RCMP TSSIT OPS-II",
            origin: "Royal Canadian Mounted Police",
            description: "Six alternating fixed passes followed by a verified \
                          random pass.",
            passes: vec![
                Pass::zeros(),
                Pass::ones(),
                Pass::zeros(),
                Pass::ones(),
                Pass::zeros(),
                Pass::ones(),
                Pass::random(),
            ],
            default_verify: VerifyMode::LastPass,
            recommended: false,
        },
        Scheme {
            id: "hmg-is5",
            name: "HMG IS5 Enhanced",
            origin: "UK CESG / NCSC",
            description: "Zeros, ones, then verified random data.",
            passes: vec![Pass::zeros(), Pass::ones(), Pass::random()],
            default_verify: VerifyMode::LastPass,
            recommended: false,
        },
        Scheme {
            id: "afssi-5020",
            name: "AFSSI-5020",
            origin: "US Air Force System Security Instruction 5020",
            description: "Zeros, ones, then a verified pseudorandom pass.",
            passes: vec![Pass::zeros(), Pass::ones(), Pass::random()],
            default_verify: VerifyMode::LastPass,
            recommended: false,
        },
        Scheme {
            id: "zero",
            name: "Quick Zero Fill",
            origin: "—",
            description: "Single unverified pass of zeros. Fast blanking for \
                          non-sensitive media; prefer NIST Clear when it matters.",
            passes: vec![Pass::zeros()],
            default_verify: VerifyMode::None,
            recommended: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_sane() {
        let schemes = all_schemes();
        assert!(schemes.len() >= 10);
        // Exactly one recommended default.
        assert_eq!(schemes.iter().filter(|s| s.recommended).count(), 1);
        // Unique ids.
        let mut ids: Vec<_> = schemes.iter().map(|s| s.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), schemes.len());
        // Every scheme has at least one pass and a non-empty description.
        for s in &schemes {
            assert!(!s.passes.is_empty(), "{} has no passes", s.id);
            assert!(!s.description.is_empty());
        }
    }

    #[test]
    fn gutmann_is_35_passes() {
        let schemes = all_schemes();
        let g = schemes.iter().find(|s| s.id == "gutmann").unwrap();
        assert_eq!(g.pass_count(), 35);
        let randoms = g
            .passes
            .iter()
            .filter(|p| p.kind == PassKind::Random)
            .count();
        assert_eq!(
            randoms, 8,
            "Gutmann has 4 leading + 4 trailing random passes"
        );
        // First four and last four are random, middle 27 are fixed.
        assert!(g.passes[..4].iter().all(|p| p.kind == PassKind::Random));
        assert!(g.passes[31..].iter().all(|p| p.kind == PassKind::Random));
    }

    #[test]
    fn dod_variants() {
        let schemes = all_schemes();
        assert_eq!(
            schemes
                .iter()
                .find(|s| s.id == "dod-short")
                .unwrap()
                .pass_count(),
            3
        );
        assert_eq!(
            schemes
                .iter()
                .find(|s| s.id == "dod-ece")
                .unwrap()
                .pass_count(),
            7
        );
        assert_eq!(
            schemes
                .iter()
                .find(|s| s.id == "schneier")
                .unwrap()
                .pass_count(),
            7
        );
        assert_eq!(
            schemes
                .iter()
                .find(|s| s.id == "vsitr")
                .unwrap()
                .pass_count(),
            7
        );
    }

    #[test]
    fn pass_labels() {
        assert_eq!(Pass::zeros().label(), "zeros (0x00)");
        assert_eq!(Pass::ones().label(), "ones (0xFF)");
        assert_eq!(Pass::random().label(), "random");
        assert_eq!(Pass::fill(&[0x92, 0x49, 0x24]).label(), "0x92 49 24");
    }

    #[test]
    fn verify_mode_cycles() {
        let mut v = VerifyMode::None;
        v = v.cycle();
        assert_eq!(v, VerifyMode::LastPass);
        v = v.cycle();
        assert_eq!(v, VerifyMode::AllPasses);
        v = v.cycle();
        assert_eq!(v, VerifyMode::None);
    }
}
