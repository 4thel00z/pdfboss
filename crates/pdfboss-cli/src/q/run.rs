//! Compiling and running jq programs (via the jaq engine) over the value
//! tree. Compile errors are reported with byte positions and become exit
//! code 2 in `cmd_q` (Task 7), distinct from PDF errors (exit code 1).

use std::fmt::Write as _;
use std::io::Write as _;

use jaq_core::data::JustLut;
use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, Vars};
use jaq_json::{Num, Val};
use pdfboss_core::elements::Span;
use pdfboss_core::Stream;
use serde_json::Value;

use crate::hexdump::{hexdump, HexOpts};
use crate::input::{use_color, Input};
use crate::json::write_json_pretty;
use crate::q::value::{build_tree, TreeFlags};
use crate::Failure;

/// A compiled jq program, ready to run over any number of inputs.
pub struct Program {
    filter: jaq_core::Filter<JustLut<Val>>,
}

impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Program").finish_non_exhaustive()
    }
}

/// Compiles `code` against the jq standard library, reporting lex/parse/
/// compile errors with byte positions.
pub fn compile_program(code: &str) -> Result<Program, String> {
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs());
    let funs = jaq_core::funs()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs());
    let loader = Loader::new(defs);
    let arena = Arena::default();
    let modules = loader
        .load(&arena, File { path: (), code })
        .map_err(|errors| describe_load_errors(code, errors))?;
    let filter = Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|errors| describe_compile_errors(code, errors))?;
    Ok(Program { filter })
}

/// Runs the program over one input value, collecting every output in order.
/// Runtime errors (e.g. `error("boom")`) come back as `Err` items.
pub fn run_program(program: &Program, input: Value) -> Vec<Result<Value, String>> {
    let input = match serde_json::from_value::<Val>(input) {
        Ok(val) => val,
        Err(e) => return vec![Err(format!("{e}"))],
    };
    let ctx = Ctx::<JustLut<Val>>::new(&program.filter.lut, Vars::new([]));
    program
        .filter
        .id
        .run((ctx, input))
        .map(|item| {
            item.map(|val| json_value(&val))
                .map_err(|exn| match exn.get_err() {
                    Ok(error) => format!("{error}"),
                    Err(exn) => match exn.get_halt() {
                        Ok(code) => format!("halt({code})"),
                        Err(_) => "jq: filter interrupted".to_string(),
                    },
                })
        })
        .collect()
}

/// The JSON value a jaq value denotes. Floats without a JSON representation
/// (infinities, NaN) become `null`, as stock jq prints them; byte strings
/// and non-string object keys are rendered as text.
fn json_value(val: &Val) -> Value {
    match val {
        Val::Null => Value::Null,
        Val::Bool(b) => Value::Bool(*b),
        Val::Num(num) => json_number(num),
        Val::TStr(bytes) | Val::BStr(bytes) => {
            Value::String(String::from_utf8_lossy(bytes).into_owned())
        }
        Val::Arr(items) => Value::Array(items.iter().map(json_value).collect()),
        Val::Obj(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, value)| (json_key(key), json_value(value)))
                .collect(),
        ),
    }
}

fn json_key(key: &Val) -> String {
    match json_value(key) {
        Value::String(text) => text,
        other => other.to_string(),
    }
}

fn json_number(num: &Num) -> Value {
    match num {
        Num::Int(i) => Value::from(*i as i64),
        Num::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
        Num::BigInt(big) => big
            .to_string()
            .parse::<serde_json::Number>()
            .map_or(Value::Null, Value::Number),
        Num::Dec(text) => text
            .parse::<serde_json::Number>()
            .map_or(Value::Null, Value::Number),
    }
}

/// Byte offset of `part` (a slice borrowed from `code`) within `code`.
fn offset_in(code: &str, part: &str) -> usize {
    (part.as_ptr() as usize).saturating_sub(code.as_ptr() as usize)
}

fn describe_load_errors(code: &str, errors: jaq_core::load::Errors<&str, ()>) -> String {
    let mut out = String::new();
    for (file, error) in errors {
        let _ = file;
        match error {
            jaq_core::load::Error::Io(items) => {
                for (path, message) in items {
                    push_error(&mut out, &format!("io error ({path}): {message}"));
                }
            }
            jaq_core::load::Error::Lex(items) => {
                for (expected, found) in items {
                    push_error(
                        &mut out,
                        &format!(
                            "lex error at byte {}: expected {}",
                            offset_in(code, found),
                            expected.as_str()
                        ),
                    );
                }
            }
            jaq_core::load::Error::Parse(items) => {
                for (expected, found) in items {
                    push_error(
                        &mut out,
                        &format!(
                            "parse error at byte {}: expected {}",
                            offset_in(code, found),
                            expected.as_str()
                        ),
                    );
                }
            }
        }
    }
    if out.is_empty() {
        out.push_str("jq: invalid program");
    }
    out
}

fn describe_compile_errors(code: &str, errors: jaq_core::compile::Errors<&str, ()>) -> String {
    let mut out = String::new();
    for (file, file_errors) in errors {
        let _ = file;
        for (found, undefined) in file_errors {
            push_error(
                &mut out,
                &format!(
                    "compile error at byte {}: undefined {}",
                    offset_in(code, found),
                    undefined.as_str()
                ),
            );
        }
    }
    if out.is_empty() {
        out.push_str("jq: invalid program");
    }
    out
}

fn push_error(out: &mut String, message: &str) {
    if !out.is_empty() {
        out.push_str("; ");
    }
    let _ = write!(out, "jq: {message}");
}

/// `pdfboss q <file-or-url> '<program>' [--raw|--decode] [--hex] [-r]
/// [--pages ..]`: run a jq program over the value tree. Compile errors exit
/// 2; PDF/IO and jq runtime errors exit 1.
///
/// `run_program` may yield a runtime `error(...)` as an `Err` item among
/// `Ok` items (a program such as `(1, error("x"), 2)` produces all three).
/// This loop stops at the first `Err`, rendering nothing after it, matching
/// stock jq's behavior of aborting the whole pipeline on a runtime error.
///
/// Also note: jaq follows IEEE 754 float semantics, so `1/0` evaluates to
/// `Float(inf)`, which `serde_json` has no representation for and which
/// therefore serializes as `null` — not a runtime error, unlike stock jq's
/// `number (0) and number (0) cannot be divided because the divisor is
/// zero`.
pub fn cmd_q(
    input_spec: &str,
    program: &str,
    flags: &TreeFlags,
    hex: bool,
    raw_strings: bool,
    password: &str,
) -> Result<(), Failure> {
    // Compile first: a bad program should fail fast, before any I/O.
    let program = compile_program(program).map_err(Failure::program)?;
    let input = Input::open_with_password(input_spec, password).map_err(Failure::new)?;
    let opts = flags.element_opts().map_err(Failure::new)?;
    let elements = input.collect_elements(opts);
    let mut decode = |s: &Stream| input.decode_stream(s);
    let tree = build_tree(
        &elements,
        flags.stream_data(),
        flags.content_ops,
        &mut decode,
    );
    let results = run_program(&program, tree);

    let color = use_color();
    let stdout = std::io::stdout();
    let mut w = std::io::BufWriter::new(stdout.lock());
    let file_len = input.file_len();
    for result in results {
        let value = result.map_err(|message| Failure::new(format!("jq: {message}")))?;
        if hex {
            if let Some(spans) = result_spans(&value) {
                for span in spans {
                    // A jq program can fabricate a `_span` that doesn't
                    // correspond to any real element. `Input::read_span`
                    // diverges by backend on out-of-range spans (the local
                    // fast path errors, the remote/aio path silently clamps
                    // to the file length), so validate here first — same
                    // guard `cmd_hex` applies to its selector-resolved
                    // spans — to fail the same clear way on both backends.
                    if span.start > file_len || span.end > file_len {
                        return Err(Failure::new(format!(
                            "_span result {}..{} lies outside the file ({file_len} bytes)",
                            span.start, span.end
                        )));
                    }
                    let bytes = input.read_span(span).map_err(Failure::new)?;
                    writeln!(w, "── {:#x}..{:#x} ──", span.start, span.end).map_err(io_failure)?;
                    let hex_opts = HexOpts { width: 16, color };
                    hexdump(&mut w, &bytes, span.start, &hex_opts).map_err(io_failure)?;
                }
                continue;
            }
        }
        if raw_strings {
            if let Value::String(s) = &value {
                writeln!(w, "{s}").map_err(io_failure)?;
                continue;
            }
        }
        let mut text = String::new();
        write_json_pretty(&mut text, &value, 0, color);
        writeln!(w, "{text}").map_err(io_failure)?;
    }
    Ok(())
}

fn io_failure(e: std::io::Error) -> Failure {
    Failure::new(e.to_string())
}

/// For `--hex`: if `v` is an object with a two-element numeric `_span`, or a
/// non-empty array made entirely of such objects, the spans to hexdump.
fn result_spans(v: &Value) -> Option<Vec<Span>> {
    fn one(v: &Value) -> Option<Span> {
        let span = v.as_object()?.get("_span")?.as_array()?;
        if span.len() != 2 {
            return None;
        }
        let start = span[0].as_u64()?;
        let end = span[1].as_u64()?;
        (end >= start).then_some(Span { start, end })
    }
    match v {
        Value::Object(_) => one(v).map(|span| vec![span]),
        Value::Array(items) if !items.is_empty() => {
            items.iter().map(one).collect::<Option<Vec<Span>>>()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::elements::Span;
    use serde_json::json;

    #[test]
    fn identity_program_round_trips() {
        let program = compile_program(".").expect("identity compiles");
        let input = json!({"a": 1});
        assert_eq!(run_program(&program, input.clone()), vec![Ok(input)]);
    }

    #[test]
    fn programs_can_produce_multiple_outputs() {
        let program = compile_program(".[] | . + 1").expect("compiles");
        assert_eq!(
            run_program(&program, json!([1, 2])),
            vec![Ok(json!(2)), Ok(json!(3))]
        );
    }

    #[test]
    fn field_and_index_access_work_over_objects() {
        let program = compile_program(r#".objects["12 0"]._span"#).expect("compiles");
        let input = json!({"objects": {"12 0": {"_span": [1, 2]}}});
        assert_eq!(run_program(&program, input), vec![Ok(json!([1, 2]))]);
    }

    #[test]
    fn std_library_functions_are_available() {
        let program = compile_program("[.[] | select(. > 1)] | length").expect("std defs loaded");
        assert_eq!(run_program(&program, json!([1, 2, 3])), vec![Ok(json!(2))]);
    }

    #[test]
    fn parse_error_reports_byte_position_with_jq_prefix() {
        let err = compile_program(".foo|").expect_err("trailing pipe is invalid");
        assert!(err.starts_with("jq:"), "no jq prefix in: {err}");
        assert!(err.contains("byte"), "no position in: {err}");
    }

    #[test]
    fn undefined_names_are_compile_errors() {
        let err = compile_program("nosuchfilter").expect_err("undefined filter");
        assert!(err.contains("undefined"), "wrong message: {err}");
    }

    #[test]
    fn runtime_errors_come_back_as_err_items() {
        let program = compile_program(r#"error("boom")"#).expect("compiles");
        let out = run_program(&program, json!(null));
        // jaq-std 3 defines `error(msgs)` as `(msgs | error_empty) as $x | .`,
        // so the error is the only stream item, as in stock jq.
        assert_eq!(out.len(), 1);
        let err = out[0].as_ref().expect_err("runtime error expected");
        assert!(err.contains("boom"), "message lost: {err}");
    }

    #[test]
    fn outputs_before_a_runtime_error_are_kept() {
        let program = compile_program(r#"(1, error("x"), 2)"#).expect("compiles");
        let out = run_program(&program, json!(null));
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], Ok(json!(1)));
        assert!(out[1].is_err());
        assert_eq!(out[2], Ok(json!(2)));
    }

    #[test]
    fn values_round_trip_through_jaq() {
        let program = compile_program(".").expect("compiles");
        let input = json!({"n": 1, "f": 1.5, "s": "text", "b": true, "z": null, "a": [1, [2]]});
        assert_eq!(run_program(&program, input.clone()), vec![Ok(input)]);
    }

    #[test]
    fn infinities_print_as_null_like_jq() {
        let program = compile_program("1 / 0").expect("compiles");
        assert_eq!(run_program(&program, json!(null)), vec![Ok(json!(null))]);
    }

    #[test]
    fn span_objects_are_detected_for_hex_mode() {
        let one = json!({"_span": [10, 20], "_kind": "object"});
        assert_eq!(result_spans(&one), Some(vec![Span { start: 10, end: 20 }]));
        let many = json!([{"_span": [0, 5]}, {"_span": [5, 9]}]);
        assert_eq!(
            result_spans(&many),
            Some(vec![Span { start: 0, end: 5 }, Span { start: 5, end: 9 }])
        );
    }

    #[test]
    fn non_span_results_fall_back_to_json() {
        assert_eq!(result_spans(&json!(42)), None);
        assert_eq!(result_spans(&json!({"span": [1, 2]})), None);
        assert_eq!(result_spans(&json!({"_span": [1]})), None);
        assert_eq!(result_spans(&json!({"_span": ["a", "b"]})), None);
        assert_eq!(result_spans(&json!({"_span": [9, 5]})), None);
        assert_eq!(result_spans(&json!([])), None);
        assert_eq!(
            result_spans(&json!([{"_span": [0, 5]}, 7])),
            None,
            "mixed arrays are not hexdumped"
        );
    }

    fn fixture(name: &str) -> String {
        format!(
            "{}/../../tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        )
    }

    fn no_flags() -> TreeFlags {
        TreeFlags {
            raw: false,
            decode: false,
            pages: None,
            no_logical: false,
            content_ops: false,
        }
    }

    // A jq program is free to fabricate a `_span` that doesn't correspond to
    // any real element (e.g. by doing arithmetic on a genuine span's
    // fields). `cmd_q --hex` must reject such an out-of-range span itself,
    // the same way `cmd_hex` guards its selector-resolved spans, rather than
    // relying on `Input::read_span`'s backend-dependent behavior (errors
    // locally, silently clamps remotely).
    #[test]
    fn hex_rejects_a_fabricated_out_of_range_span() {
        let path = fixture("hello.pdf");
        let len = std::fs::metadata(&path).expect("fixture exists").len();
        let failure = cmd_q(
            &path,
            r#"{"_span": [0, 999999]}"#,
            &no_flags(),
            true,
            false,
            "",
        )
        .expect_err("fabricated span exceeds the file length");
        assert_eq!(failure.code, 1);
        assert!(
            failure.message.contains("_span result"),
            "not the guard's own message: {}",
            failure.message
        );
        assert!(
            failure.message.contains(&len.to_string()),
            "error does not mention file length {len}: {}",
            failure.message
        );
    }
}
