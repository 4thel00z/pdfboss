//! `pdfboss q`: document to JSON value tree, and the jq engine that queries it.

// `run` is consumed by the `pdfboss q` subcommand wiring (Task 7); the
// `dead_code` allowance disappears once it is wired up.
#[allow(dead_code)]
pub mod run;
pub mod value;
