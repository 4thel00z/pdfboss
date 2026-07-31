//! Env-gated end-to-end checks over real-world PDF-embedded codestreams.
//!
//! The streams come from corpus PDFs and are never committed (provenance:
//! they were extracted from third-party documents, so they stay outside
//! the repository). Point `PDFBOSS_JPX_REAL_DIR` at a directory holding
//! them to run the assertions; when the variable is unset, or the
//! directory or a file is missing, the test passes trivially with a note
//! — CI has no streams.
//!
//! Why these streams matter: a widely-used independent codec rejects them
//! because it treats TNsot (the declared tile-part count) as binding.
//! T.800 Table A.6 makes TNsot informational and A.4.2 does not license
//! rejection, so decoding them IS the point of this crate's leniency
//! doctrine. The expected dimensions below were verified against the SIZ
//! bytes (T.800 A.5.1) independently of any decoder.

use pdfboss_jpx::{decode, ColorKind, DecodeLimits};
use std::path::PathBuf;

struct RealCase {
    file: &'static str,
    width: u32,
    height: u32,
    components: u8,
}

const CASES: [RealCase; 3] = [
    RealCase {
        file: "real-049124-0.jp2",
        width: 437,
        height: 130,
        components: 3,
    },
    RealCase {
        file: "real-049124-2.jp2",
        width: 915,
        height: 14,
        components: 3,
    },
    RealCase {
        file: "real-049359-0.jp2",
        width: 958,
        height: 547,
        components: 3,
    },
];

/// Warnings these streams are allowed to carry: the skipped
/// reader-requirements box and the one-per-codestream TNsot advisory.
/// Anything else is a regression.
fn warning_allowed(warning: &str) -> bool {
    warning == "reader-requirements (rreq) box skipped"
        || warning.ends_with(
            "tile(s) ship more tile-parts than their declared TNsot; \
             the count is advisory (T.800 A.4.2)",
        )
}

#[test]
fn real_world_streams_decode_within_the_documented_warnings() {
    let Some(dir) = std::env::var_os("PDFBOSS_JPX_REAL_DIR") else {
        eprintln!("PDFBOSS_JPX_REAL_DIR unset; skipping the real-world stream checks");
        return;
    };
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        eprintln!("PDFBOSS_JPX_REAL_DIR is not a directory ({dir:?}); skipping");
        return;
    }
    for case in &CASES {
        let path = dir.join(case.file);
        if !path.is_file() {
            eprintln!("{} absent under {dir:?}; skipped", case.file);
            continue;
        }
        let data = std::fs::read(&path).unwrap();
        let image = decode(&data, &DecodeLimits::default())
            .unwrap_or_else(|e| panic!("{}: decode failed: {e}", case.file));

        assert_eq!(
            (image.width, image.height),
            (case.width, case.height),
            "{}: dimensions",
            case.file
        );
        assert_eq!(
            image.components, case.components,
            "{}: component count",
            case.file
        );
        assert_eq!(image.color, ColorKind::Rgb, "{}: colour kind", case.file);
        assert_eq!(image.alpha_index, None, "{}: alpha index", case.file);

        for warning in &image.warnings {
            assert!(
                warning_allowed(warning),
                "{}: warning outside the allow-list: {warning:?}",
                case.file
            );
        }

        // Not degenerate: at least 5% of the samples must differ from the
        // first one, guarding against all-black or constant output.
        let first = image.samples[0];
        let differing = image.samples.iter().filter(|&&s| s != first).count();
        assert!(
            differing * 20 >= image.samples.len(),
            "{}: only {differing} of {} samples differ from the first — degenerate output",
            case.file,
            image.samples.len()
        );
    }
}
