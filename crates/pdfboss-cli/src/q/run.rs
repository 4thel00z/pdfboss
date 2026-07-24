//! Compiling and running jq programs (via the jaq engine) over the value
//! tree. Compile errors are reported with byte positions and become exit
//! code 2 in `cmd_q` (Task 7), distinct from PDF errors (exit code 1).

use std::fmt::Write as _;

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, RcIter};
use jaq_json::Val;
use serde_json::Value;

/// A compiled jq program, ready to run over any number of inputs.
pub struct Program {
    filter: jaq_core::Filter<jaq_core::Native<Val>>,
}

impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Program").finish_non_exhaustive()
    }
}

/// Compiles `code` against the jq standard library, reporting lex/parse/
/// compile errors with byte positions.
pub fn compile_program(code: &str) -> Result<Program, String> {
    let loader = Loader::new(jaq_std::defs().chain(jaq_json::defs()));
    let arena = Arena::default();
    let modules = loader
        .load(&arena, File { path: (), code })
        .map_err(|errors| describe_load_errors(code, errors))?;
    let filter = Compiler::default()
        .with_funs(jaq_std::funs().chain(jaq_json::funs()))
        .compile(modules)
        .map_err(|errors| describe_compile_errors(code, errors))?;
    Ok(Program { filter })
}

/// Runs the program over one input value, collecting every output in order.
/// Runtime errors (e.g. `error("boom")`) come back as `Err` items.
pub fn run_program(program: &Program, input: Value) -> Vec<Result<Value, String>> {
    let inputs = RcIter::new(core::iter::empty());
    program
        .filter
        .run((Ctx::new([], &inputs), Val::from(input)))
        .map(|item| item.map(Value::from).map_err(|e| format!("{e}")))
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
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
        // jaq-std 2.1.2 defines `error(msgs)` as
        // `((msgs | error) as $x | empty), .`: the error is one stream item,
        // but `,` does not short-circuit on an upstream error (unlike stock
        // jq's C implementation aborting the whole pipeline), so the
        // unchanged input follows as a second item. Task 7's CLI wiring
        // stops rendering after the first `Err` to match jq's observable
        // exit-1 behavior; `run_program` itself reports every stream item.
        assert_eq!(out.len(), 2);
        let err = out[0].as_ref().expect_err("runtime error expected");
        assert!(err.contains("boom"), "message lost: {err}");
        assert_eq!(out[1], Ok(json!(null)));
    }
}
