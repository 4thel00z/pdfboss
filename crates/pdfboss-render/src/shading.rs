//! Shading dictionaries (ISO 32000-1 §8.7.4.5): axial (type 2) and radial
//! (type 3) shadings evaluated through function types 0 (sampled), 2
//! (exponential), 3 (stitching) and 4 (PostScript calculator), painted per
//! device pixel under a coverage mask. Function-based (type 1) and mesh
//! (types 4-7) shadings load as `None` so the caller reports them as
//! unsupported instead of guessing.

use pdfboss_core::geom::{Matrix, Point};
use pdfboss_core::{decoded_stream_data_with, AsyncObjectSource, Dict, Error, Object};

use crate::color::ColorSpace;
use crate::raster::{paint_pixel, BlendMode, Mask};
use crate::Pixmap;

/// Most components any color space here carries (CMYK is 4; `Other` caps at
/// what `ColorSpace::to_rgb` reads).
pub(crate) const MAX_COMPS: usize = 8;

/// Upper bound on parsed function nodes per shading. A real gradient uses a
/// handful (one stitching function over a few exponentials); the cap only
/// stops a hostile file minting nodes without limit.
const MAX_FUNCTIONS: usize = 256;

/// Upper bound on a sampled function's grid (the product of its `/Size`
/// entries). A real tint or gradient table holds at most a few thousand
/// samples; the cap stops a hostile `/Size` from driving giant index math.
const MAX_SAMPLES: u64 = 1 << 24;

/// Operand-stack depth limit for calculator programs (§7.10.5.1 limits the
/// stack to 100 entries).
const CALC_STACK: usize = 100;

/// Instructions one calculator evaluation may execute before the program is
/// declared runaway and its outputs clamp to the bottom of `/Range`.
const CALC_STEPS: usize = 10_000;

/// Compiled calculator length cap: a longer program fails to load. Larger
/// than [`CALC_STEPS`] because branches skip instructions.
const MAX_CALC_OPS: usize = 65_536;

/// Brace-nesting cap for calculator programs; compilation recurses over
/// blocks, so the depth must stay bounded.
const MAX_CALC_DEPTH: usize = 32;

/// One parsed function. Stitching children are arena indices, so loading
/// needs no recursion (a queue fills the arena) and evaluation recurses
/// over indices with the depth bounded by [`MAX_FUNCTIONS`].
#[derive(Debug, Clone, PartialEq)]
enum Node {
    /// Type 2: `C0 + x^N (C1 - C0)` over `domain`.
    Exponential {
        domain: [f32; 2],
        c0: Vec<f32>,
        c1: Vec<f32>,
        n: f32,
    },
    /// Type 3: subfunction `i` covers `[bounds[i-1], bounds[i])`, its input
    /// re-mapped through `encode[2i..2i+2]`.
    Stitching {
        domain: [f32; 2],
        children: Vec<usize>,
        bounds: Vec<f32>,
        encode: Vec<f32>,
    },
    /// Type 0: `outputs` values per sample, `bps` bits each, big-endian,
    /// over a grid of `size[i]` samples per input dimension with the first
    /// dimension varying fastest, interpolated multilinearly between the
    /// 2^m nearest samples (§7.10.2). `domain` and `encode` hold two
    /// entries per dimension.
    Sampled {
        domain: Vec<f32>,
        encode: Vec<f32>,
        decode: Vec<f32>,
        size: Vec<usize>,
        bps: u32,
        outputs: usize,
        data: Vec<u8>,
    },
    /// Type 4: a compiled PostScript calculator program (§7.10.5).
    /// `domain` and `range` hold two entries per input/output; a runtime
    /// failure (stack underflow, type mismatch, runaway program) clamps
    /// every output to the bottom of its range instead of dropping the
    /// element being painted.
    Calculator {
        domain: Vec<f32>,
        range: Vec<f32>,
        program: Vec<Calc>,
    },
}

/// One value on a calculator's operand stack. The boolean/number split is
/// load-bearing: `and`, `or`, `xor` and `not` are logical on booleans and
/// bitwise on integers, and `if`/`ifelse` demand a boolean.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Value {
    Num(f32),
    Bool(bool),
}

/// One flat calculator instruction. `if`/`ifelse` compile to explicit
/// forward jumps, so evaluation is a plain loop — the language has no
/// backward edges.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Calc {
    Push(Value),
    /// Unconditional jump past an else branch.
    Jump(usize),
    /// Pops a boolean and jumps when it is false.
    JumpIfFalse(usize),
    Abs,
    Add,
    Atan,
    Ceiling,
    Cos,
    Cvi,
    Cvr,
    Div,
    Exp,
    Floor,
    Idiv,
    Ln,
    Log,
    Mod,
    Mul,
    Neg,
    Round,
    Sin,
    Sqrt,
    Sub,
    Truncate,
    And,
    Bitshift,
    Eq,
    Ge,
    Gt,
    Le,
    Lt,
    Ne,
    Not,
    Or,
    Xor,
    Copy,
    Dup,
    Exch,
    Index,
    Pop,
    Roll,
}

/// One parsed calculator token before jump resolution: a literal, a
/// resolved operator, a brace-delimited procedure, or the conditional that
/// consumes procedures.
#[derive(Debug, Clone, PartialEq)]
enum CalcItem {
    Op(Calc),
    If,
    IfElse,
    Block(Vec<CalcItem>),
}

/// The functions a shading evaluates: one n-output function, or an array of
/// single-output functions whose results concatenate (§8.7.4.5.2).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Functions {
    nodes: Vec<Node>,
    roots: Vec<usize>,
}

impl Functions {
    /// Evaluates every root at `inputs` into `out`, returning how many
    /// components were written. Missing inputs read as 0; every root sees
    /// the same inputs and their outputs concatenate.
    pub(crate) fn eval(&self, inputs: &[f32], out: &mut [f32; MAX_COMPS]) -> usize {
        let mut written = 0;
        for &root in &self.roots {
            if written >= MAX_COMPS {
                break;
            }
            written += self.eval_node(root, inputs, &mut out[written..]);
        }
        written
    }

    fn eval_node(&self, idx: usize, inputs: &[f32], out: &mut [f32]) -> usize {
        match &self.nodes[idx] {
            Node::Exponential { domain, c0, c1, n } => {
                let x = first_input(inputs).clamp(domain[0], domain[1]);
                // x^1 is the overwhelmingly common gradient; skip the powf.
                let xn = if *n == 1.0 { x } else { x.powf(*n) };
                let count = c0.len().min(out.len());
                for (i, slot) in out.iter_mut().enumerate().take(count) {
                    *slot = c0[i] + xn * (c1[i] - c0[i]);
                }
                count
            }
            Node::Stitching {
                domain,
                children,
                bounds,
                encode,
            } => {
                let x = first_input(inputs).clamp(domain[0], domain[1]);
                // Subinterval k: bounds partition [domain0, domain1).
                let k = bounds.iter().take_while(|&&b| x >= b).count();
                let Some(&child) = children.get(k) else {
                    return 0;
                };
                let lo = if k == 0 { domain[0] } else { bounds[k - 1] };
                let hi = bounds.get(k).copied().unwrap_or(domain[1]);
                let (e0, e1) = (encode[2 * k], encode[2 * k + 1]);
                let t = if hi > lo { (x - lo) / (hi - lo) } else { 0.0 };
                self.eval_node(child, &[e0 + t * (e1 - e0)], out)
            }
            Node::Sampled {
                domain,
                encode,
                decode,
                size,
                bps,
                outputs,
                data,
            } => {
                let m = size.len();
                // Grid neighborhood per dimension: the sample below the
                // encoded input, the one above, and the blend between them.
                let mut lo = [0usize; MAX_COMPS];
                let mut hi = [0usize; MAX_COMPS];
                let mut frac = [0f32; MAX_COMPS];
                for i in 0..m {
                    let x = inputs
                        .get(i)
                        .copied()
                        .unwrap_or(0.0)
                        .clamp(domain[2 * i], domain[2 * i + 1]);
                    let span = domain[2 * i + 1] - domain[2 * i];
                    let t = if span > 0.0 {
                        (x - domain[2 * i]) / span
                    } else {
                        0.0
                    };
                    let e = (encode[2 * i] + t * (encode[2 * i + 1] - encode[2 * i]))
                        .clamp(0.0, (size[i] - 1) as f32);
                    lo[i] = e.floor() as usize;
                    hi[i] = (lo[i] + 1).min(size[i] - 1);
                    frac[i] = e - lo[i] as f32;
                }
                let count = (*outputs).min(out.len()).min(decode.len() / 2);
                let max = ((1u64 << *bps) - 1) as f32;
                for (j, slot) in out.iter_mut().enumerate().take(count) {
                    // Multilinear blend over the 2^m corners of the
                    // neighborhood; bit i of the corner picks lo/hi in
                    // dimension i, and the first dimension varies fastest
                    // in the sample stream.
                    let mut acc = 0.0f32;
                    for corner in 0..1usize << m {
                        let mut weight = 1.0f32;
                        let mut index = 0u64;
                        let mut stride = 1u64;
                        for i in 0..m {
                            let up = corner & (1 << i) != 0;
                            weight *= if up { frac[i] } else { 1.0 - frac[i] };
                            index += if up { hi[i] as u64 } else { lo[i] as u64 } * stride;
                            stride *= size[i] as u64;
                        }
                        if weight <= 0.0 {
                            continue;
                        }
                        acc += weight
                            * sample_at(data, index * *outputs as u64 + j as u64, *bps) as f32;
                    }
                    let s = acc / max;
                    *slot = decode[2 * j] + s * (decode[2 * j + 1] - decode[2 * j]);
                }
                count
            }
            Node::Calculator {
                domain,
                range,
                program,
            } => {
                let m = domain.len() / 2;
                let outputs = range.len() / 2;
                let count = outputs.min(out.len());
                let mut clamped = [0f32; MAX_COMPS];
                for (i, slot) in clamped.iter_mut().enumerate().take(m) {
                    *slot = inputs
                        .get(i)
                        .copied()
                        .unwrap_or(0.0)
                        .clamp(domain[2 * i], domain[2 * i + 1]);
                }
                // The program's results are the top `outputs` stack values,
                // bottom-most first; a missing, boolean, or non-finite
                // result fails the whole evaluation.
                let results = run_calculator(program, &clamped[..m])
                    .filter(|stack| stack.len() >= outputs)
                    .and_then(|stack| {
                        let start = stack.len() - outputs;
                        let mut values = [0f32; MAX_COMPS];
                        for (j, slot) in values.iter_mut().enumerate().take(count) {
                            match stack[start + j] {
                                Value::Num(v) if v.is_finite() => *slot = v,
                                _ => return None,
                            }
                        }
                        Some(values)
                    })
                    .unwrap_or([0f32; MAX_COMPS]);
                for (j, slot) in out.iter_mut().enumerate().take(count) {
                    // max/min instead of clamp: a reversed range must not
                    // panic, just pin to its own bounds.
                    *slot = results[j].max(range[2 * j]).min(range[2 * j + 1]);
                }
                count
            }
        }
    }
}

/// The single input the 1-input function types read, 0 when absent.
fn first_input(inputs: &[f32]) -> f32 {
    inputs.first().copied().unwrap_or(0.0)
}

/// Reads big-endian sample `index` of `bps` bits from a packed bit stream,
/// 0 when the data ends early (the truncated region reads as zero samples,
/// the same leniency images get).
fn sample_at(data: &[u8], index: u64, bps: u32) -> u64 {
    let mut value = 0u64;
    let start = index * bps as u64;
    for bit in start..start + bps as u64 {
        let byte = (bit / 8) as usize;
        let within = 7 - (bit % 8) as u32;
        let b = data.get(byte).copied().unwrap_or(0);
        value = (value << 1) | u64::from((b >> within) & 1);
    }
    value
}

/// The item a calculator token names, or `None` for anything that has to
/// be a number.
fn calc_item(token: &str) -> Option<CalcItem> {
    let op = match token {
        "abs" => Calc::Abs,
        "add" => Calc::Add,
        "atan" => Calc::Atan,
        "ceiling" => Calc::Ceiling,
        "cos" => Calc::Cos,
        "cvi" => Calc::Cvi,
        "cvr" => Calc::Cvr,
        "div" => Calc::Div,
        "exp" => Calc::Exp,
        "floor" => Calc::Floor,
        "idiv" => Calc::Idiv,
        "ln" => Calc::Ln,
        "log" => Calc::Log,
        "mod" => Calc::Mod,
        "mul" => Calc::Mul,
        "neg" => Calc::Neg,
        "round" => Calc::Round,
        "sin" => Calc::Sin,
        "sqrt" => Calc::Sqrt,
        "sub" => Calc::Sub,
        "truncate" => Calc::Truncate,
        "and" => Calc::And,
        "bitshift" => Calc::Bitshift,
        "eq" => Calc::Eq,
        "ge" => Calc::Ge,
        "gt" => Calc::Gt,
        "le" => Calc::Le,
        "lt" => Calc::Lt,
        "ne" => Calc::Ne,
        "not" => Calc::Not,
        "or" => Calc::Or,
        "xor" => Calc::Xor,
        "copy" => Calc::Copy,
        "dup" => Calc::Dup,
        "exch" => Calc::Exch,
        "index" => Calc::Index,
        "pop" => Calc::Pop,
        "roll" => Calc::Roll,
        "true" => Calc::Push(Value::Bool(true)),
        "false" => Calc::Push(Value::Bool(false)),
        "if" => return Some(CalcItem::If),
        "ifelse" => return Some(CalcItem::IfElse),
        _ => return None,
    };
    Some(CalcItem::Op(op))
}

/// Parses the decoded bytes of a type 4 program: one brace-delimited block,
/// optionally surrounded by whitespace or `%` comments. Anything else —
/// unbalanced braces, an unknown operator, trailing tokens — is a load
/// error, so the caller's report machinery fires.
fn parse_calculator(data: &[u8]) -> Result<Vec<CalcItem>, Error> {
    let malformed = |what: &str| Error::Other(format!("calculator program {what}"));
    let mut blocks: Vec<Vec<CalcItem>> = Vec::new();
    let mut done: Option<Vec<CalcItem>> = None;
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ' => i += 1,
            b'%' => {
                while i < data.len() && !matches!(data[i], b'\n' | b'\r') {
                    i += 1;
                }
            }
            b'{' => {
                if done.is_some() {
                    return Err(malformed("continues after its closing brace"));
                }
                if blocks.len() >= MAX_CALC_DEPTH {
                    return Err(malformed("nests too deeply"));
                }
                blocks.push(Vec::new());
                i += 1;
            }
            b'}' => {
                let block = blocks
                    .pop()
                    .ok_or_else(|| malformed("has unbalanced braces"))?;
                match blocks.last_mut() {
                    Some(parent) => parent.push(CalcItem::Block(block)),
                    None => done = Some(block),
                }
                i += 1;
            }
            _ => {
                if done.is_some() {
                    return Err(malformed("continues after its closing brace"));
                }
                let start = i;
                while i < data.len()
                    && !data[i].is_ascii_whitespace()
                    && !matches!(data[i], b'\0' | b'{' | b'}' | b'%')
                {
                    i += 1;
                }
                let token = std::str::from_utf8(&data[start..i])
                    .map_err(|_| malformed("holds a non-ASCII token"))?;
                let dest = blocks
                    .last_mut()
                    .ok_or_else(|| malformed("has tokens outside the braces"))?;
                match calc_item(token) {
                    Some(item) => dest.push(item),
                    None => {
                        if !matches!(token.as_bytes()[0], b'0'..=b'9' | b'+' | b'-' | b'.') {
                            return Err(Error::Other(format!(
                                "calculator operator {token} unknown"
                            )));
                        }
                        let number: f32 = token.parse().map_err(|_| {
                            Error::Other(format!("calculator number {token} unusable"))
                        })?;
                        dest.push(CalcItem::Op(Calc::Push(Value::Num(number))));
                    }
                }
            }
        }
    }
    done.ok_or_else(|| malformed("has unbalanced braces"))
}

/// Flattens parsed items into instructions. A procedure block is legal only
/// as the operand of `if`/`ifelse` (§7.10.5.4), where it becomes a forward
/// jump over the branch body.
fn compile_calculator(items: &[CalcItem], out: &mut Vec<Calc>) -> Result<(), Error> {
    let mut i = 0;
    while i < items.len() {
        if out.len() > MAX_CALC_OPS {
            return Err(Error::Other("calculator program too long".into()));
        }
        match &items[i] {
            CalcItem::Op(op) => out.push(*op),
            CalcItem::If | CalcItem::IfElse => {
                return Err(Error::Other(
                    "calculator conditional lacks its procedure".into(),
                ))
            }
            CalcItem::Block(body) => match (items.get(i + 1), items.get(i + 2)) {
                (Some(CalcItem::If), _) => {
                    let skip = out.len();
                    out.push(Calc::JumpIfFalse(0));
                    compile_calculator(body, out)?;
                    out[skip] = Calc::JumpIfFalse(out.len());
                    i += 2;
                    continue;
                }
                (Some(CalcItem::Block(other)), Some(CalcItem::IfElse)) => {
                    let skip = out.len();
                    out.push(Calc::JumpIfFalse(0));
                    compile_calculator(body, out)?;
                    let done = out.len();
                    out.push(Calc::Jump(0));
                    out[skip] = Calc::JumpIfFalse(out.len());
                    compile_calculator(other, out)?;
                    out[done] = Calc::Jump(out.len());
                    i += 3;
                    continue;
                }
                _ => {
                    return Err(Error::Other(
                        "calculator procedure without if or ifelse".into(),
                    ))
                }
            },
        }
        i += 1;
    }
    if out.len() > MAX_CALC_OPS {
        return Err(Error::Other("calculator program too long".into()));
    }
    Ok(())
}

/// The integer a calculator number stands for; integer-only operators
/// truncate toward zero and refuse non-finite operands.
fn calc_int(v: f32) -> Option<i32> {
    if !v.is_finite() {
        return None;
    }
    Some(v.trunc() as i32)
}

/// Pushes with the §7.10.5.1 depth cap.
fn calc_push(stack: &mut Vec<Value>, v: Value) -> Option<()> {
    if stack.len() >= CALC_STACK {
        return None;
    }
    stack.push(v);
    Some(())
}

/// Pops a number; a boolean operand is a type error.
fn calc_num(stack: &mut Vec<Value>) -> Option<f32> {
    match stack.pop()? {
        Value::Num(v) => Some(v),
        Value::Bool(_) => None,
    }
}

/// Replaces the top two numbers with `f` of them; `None` from `f` is a
/// domain error (division by zero, log of a non-positive number).
fn calc_binary(stack: &mut Vec<Value>, f: impl Fn(f32, f32) -> Option<f32>) -> Option<()> {
    let b = calc_num(stack)?;
    let a = calc_num(stack)?;
    stack.push(Value::Num(f(a, b)?));
    Some(())
}

/// Replaces the top number with `f` of it.
fn calc_unary(stack: &mut Vec<Value>, f: impl Fn(f32) -> Option<f32>) -> Option<()> {
    let a = calc_num(stack)?;
    stack.push(Value::Num(f(a)?));
    Some(())
}

/// Replaces the top two numbers with their comparison.
fn calc_compare(stack: &mut Vec<Value>, f: impl Fn(f32, f32) -> bool) -> Option<()> {
    let b = calc_num(stack)?;
    let a = calc_num(stack)?;
    stack.push(Value::Bool(f(a, b)));
    Some(())
}

/// `and`/`or`/`xor`: bitwise over two integers, logical over two booleans,
/// a type error over a mix.
fn calc_logic(
    stack: &mut Vec<Value>,
    ints: impl Fn(i32, i32) -> i32,
    bools: impl Fn(bool, bool) -> bool,
) -> Option<()> {
    let b = stack.pop()?;
    let a = stack.pop()?;
    let v = match (a, b) {
        (Value::Num(a), Value::Num(b)) => Value::Num(ints(calc_int(a)?, calc_int(b)?) as f32),
        (Value::Bool(a), Value::Bool(b)) => Value::Bool(bools(a, b)),
        _ => return None,
    };
    stack.push(v);
    Some(())
}

/// Whether two calculator values are equal; values of different types
/// compare unequal rather than erroring.
fn calc_equal(stack: &mut Vec<Value>) -> Option<bool> {
    let b = stack.pop()?;
    let a = stack.pop()?;
    let equal = match (a, b) {
        (Value::Num(a), Value::Num(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        _ => false,
    };
    Some(equal)
}

/// Runs a compiled calculator over `inputs`. `None` is any runtime failure
/// — stack underflow or overflow, a type mismatch, a domain error, or a
/// runaway program — which the caller paints as range-clamped zeros.
fn run_calculator(program: &[Calc], inputs: &[f32]) -> Option<Vec<Value>> {
    if inputs.len() > CALC_STACK {
        return None;
    }
    let mut stack: Vec<Value> = inputs.iter().map(|&v| Value::Num(v)).collect();
    let mut pc = 0;
    let mut steps = 0;
    while let Some(op) = program.get(pc) {
        steps += 1;
        if steps > CALC_STEPS {
            return None;
        }
        pc += 1;
        match *op {
            Calc::Push(v) => calc_push(&mut stack, v)?,
            Calc::Jump(target) => pc = target,
            Calc::JumpIfFalse(target) => match stack.pop()? {
                Value::Bool(true) => {}
                Value::Bool(false) => pc = target,
                Value::Num(_) => return None,
            },
            Calc::Abs => calc_unary(&mut stack, |a| Some(a.abs()))?,
            Calc::Add => calc_binary(&mut stack, |a, b| Some(a + b))?,
            Calc::Atan => calc_binary(&mut stack, |num, den| {
                let deg = num.atan2(den).to_degrees();
                Some(if deg < 0.0 { deg + 360.0 } else { deg })
            })?,
            Calc::Ceiling => calc_unary(&mut stack, |a| Some(a.ceil()))?,
            Calc::Cos => calc_unary(&mut stack, |a| Some(a.to_radians().cos()))?,
            Calc::Cvi => calc_unary(&mut stack, |a| calc_int(a).map(|i| i as f32))?,
            Calc::Cvr => calc_unary(&mut stack, Some)?,
            Calc::Div => calc_binary(&mut stack, |a, b| (b != 0.0).then(|| a / b))?,
            Calc::Exp => calc_binary(&mut stack, |a, b| Some(a.powf(b)))?,
            Calc::Floor => calc_unary(&mut stack, |a| Some(a.floor()))?,
            Calc::Idiv => calc_binary(&mut stack, |a, b| {
                let (a, b) = (calc_int(a)?, calc_int(b)?);
                (b != 0).then(|| a.wrapping_div(b) as f32)
            })?,
            Calc::Ln => calc_unary(&mut stack, |a| (a > 0.0).then(|| a.ln()))?,
            Calc::Log => calc_unary(&mut stack, |a| (a > 0.0).then(|| a.log10()))?,
            Calc::Mod => calc_binary(&mut stack, |a, b| {
                let (a, b) = (calc_int(a)?, calc_int(b)?);
                (b != 0).then(|| a.wrapping_rem(b) as f32)
            })?,
            Calc::Mul => calc_binary(&mut stack, |a, b| Some(a * b))?,
            Calc::Neg => calc_unary(&mut stack, |a| Some(-a))?,
            // Ties go to the greater integer, so this is not f32::round.
            Calc::Round => calc_unary(&mut stack, |a| Some((a + 0.5).floor()))?,
            Calc::Sin => calc_unary(&mut stack, |a| Some(a.to_radians().sin()))?,
            Calc::Sqrt => calc_unary(&mut stack, |a| (a >= 0.0).then(|| a.sqrt()))?,
            Calc::Sub => calc_binary(&mut stack, |a, b| Some(a - b))?,
            Calc::Truncate => calc_unary(&mut stack, |a| Some(a.trunc()))?,
            Calc::And => calc_logic(&mut stack, |a, b| a & b, |a, b| a && b)?,
            Calc::Bitshift => calc_binary(&mut stack, |a, shift| {
                let (a, shift) = (calc_int(a)?, calc_int(shift)?);
                let shifted = match shift {
                    32.. => 0,
                    0..=31 => ((a as u32) << shift) as i32,
                    -31..=-1 => ((a as u32) >> -shift) as i32,
                    _ => 0,
                };
                Some(shifted as f32)
            })?,
            Calc::Eq => {
                let equal = calc_equal(&mut stack)?;
                stack.push(Value::Bool(equal));
            }
            Calc::Ne => {
                let equal = calc_equal(&mut stack)?;
                stack.push(Value::Bool(!equal));
            }
            Calc::Ge => calc_compare(&mut stack, |a, b| a >= b)?,
            Calc::Gt => calc_compare(&mut stack, |a, b| a > b)?,
            Calc::Le => calc_compare(&mut stack, |a, b| a <= b)?,
            Calc::Lt => calc_compare(&mut stack, |a, b| a < b)?,
            Calc::Not => {
                let v = match stack.pop()? {
                    Value::Bool(b) => Value::Bool(!b),
                    Value::Num(v) => Value::Num(!calc_int(v)? as f32),
                };
                stack.push(v);
            }
            Calc::Or => calc_logic(&mut stack, |a, b| a | b, |a, b| a || b)?,
            Calc::Xor => calc_logic(&mut stack, |a, b| a ^ b, |a, b| a ^ b)?,
            Calc::Copy => {
                let n = calc_int(calc_num(&mut stack)?)?;
                if n < 0 || n as usize > stack.len() || stack.len() + n as usize > CALC_STACK {
                    return None;
                }
                let start = stack.len() - n as usize;
                for i in start..start + n as usize {
                    let v = stack[i];
                    stack.push(v);
                }
            }
            Calc::Dup => {
                let v = *stack.last()?;
                calc_push(&mut stack, v)?;
            }
            Calc::Exch => {
                let b = stack.pop()?;
                let a = stack.pop()?;
                stack.push(b);
                stack.push(a);
            }
            Calc::Index => {
                let n = calc_int(calc_num(&mut stack)?)?;
                if n < 0 || n as usize >= stack.len() {
                    return None;
                }
                let v = stack[stack.len() - 1 - n as usize];
                calc_push(&mut stack, v)?;
            }
            Calc::Pop => {
                stack.pop()?;
            }
            Calc::Roll => {
                let j = calc_int(calc_num(&mut stack)?)?;
                let n = calc_int(calc_num(&mut stack)?)?;
                if n < 0 || n as usize > stack.len() {
                    return None;
                }
                if n > 0 {
                    let start = stack.len() - n as usize;
                    stack[start..].rotate_right(j.rem_euclid(n) as usize);
                }
            }
        }
    }
    Some(stack)
}

/// The geometry of a supported shading.
enum Geometry {
    /// Type 2: the axis from `p0` to `p1`.
    Axial { p0: Point, p1: Point },
    /// Type 3: circles blended from `(c0, r0)` to `(c1, r1)`.
    Radial {
        c0: Point,
        r0: f32,
        c1: Point,
        r1: f32,
    },
}

impl Geometry {
    /// The shading parameter `s` (0..=1) painting point `p`, or `None` when
    /// `p` lies outside the shading and its extensions. Extension paints the
    /// endpoint color, so out-of-range values clamp when the matching
    /// `extend` flag allows them.
    fn param_at(&self, p: Point, extend: [bool; 2]) -> Option<f32> {
        match self {
            Geometry::Axial { p0, p1 } => {
                let dx = p1.x - p0.x;
                let dy = p1.y - p0.y;
                let denom = dx * dx + dy * dy;
                if !denom.is_finite() || denom <= 0.0 {
                    return None;
                }
                let s = ((p.x - p0.x) * dx + (p.y - p0.y) * dy) / denom;
                clamp_extended(s, extend)
            }
            Geometry::Radial { c0, r0, c1, r1 } => {
                let dcx = c1.x - c0.x;
                let dcy = c1.y - c0.y;
                let dr = r1 - r0;
                let qx = p.x - c0.x;
                let qy = p.y - c0.y;
                // |p - c(s)| = r(s) as a quadratic in s (§8.7.4.5.4).
                let a = dcx * dcx + dcy * dcy - dr * dr;
                let b = -2.0 * (qx * dcx + qy * dcy + r0 * dr);
                let c = qx * qx + qy * qy - r0 * r0;
                let (lo, hi) = (
                    if extend[0] { f32::NEG_INFINITY } else { 0.0 },
                    if extend[1] { f32::INFINITY } else { 1.0 },
                );
                let mut best: Option<f32> = None;
                let mut consider = |s: f32| {
                    if !s.is_finite() || s < lo || s > hi || r0 + s * dr < 0.0 {
                        return;
                    }
                    // The largest s wins: later circles paint over earlier.
                    if best.is_none_or(|b| s > b) {
                        best = Some(s);
                    }
                };
                if a.abs() > 1e-6 {
                    let disc = b * b - 4.0 * a * c;
                    if disc < 0.0 {
                        return None;
                    }
                    let sq = disc.sqrt();
                    consider((-b + sq) / (2.0 * a));
                    consider((-b - sq) / (2.0 * a));
                } else if b.abs() > 1e-6 {
                    consider(-c / b);
                } else {
                    return None;
                }
                best.and_then(|s| clamp_extended(s, extend))
            }
        }
    }
}

/// Clamps an extended parameter to 0..=1, or rejects it where the matching
/// `/Extend` flag is off.
fn clamp_extended(s: f32, extend: [bool; 2]) -> Option<f32> {
    if !s.is_finite() {
        return None;
    }
    if s < 0.0 {
        return extend[0].then_some(0.0);
    }
    if s > 1.0 {
        return extend[1].then_some(1.0);
    }
    Some(s)
}

/// A loaded, paintable shading.
pub(crate) struct Shading {
    geometry: Geometry,
    cs: ColorSpace,
    functions: Functions,
    domain: [f32; 2],
    extend: [bool; 2],
    /// Optional clip in the shading's own target space (`/BBox`); the caller
    /// intersects it into the paint region under the same matrix the shading
    /// paints with.
    pub(crate) bbox: Option<[f32; 4]>,
    /// `/Background`, resolved to RGB — painted behind a shading *pattern*
    /// fill (never behind `sh`, §8.7.4.3).
    pub(crate) background: Option<[f32; 3]>,
}

/// Reads the first `n` finite numbers of a (possibly indirect) array.
async fn floats<S: AsyncObjectSource>(src: &S, obj: Option<&Object>, n: usize) -> Option<Vec<f32>> {
    let arr = match src.resolve(obj?).await {
        Ok(Object::Array(a)) if a.len() >= n => a,
        _ => return None,
    };
    let mut out = Vec::with_capacity(n);
    for o in arr.iter().take(n) {
        match src.resolve(o).await {
            Ok(v) => match v.as_f64() {
                Some(f) if (f as f32).is_finite() => out.push(f as f32),
                _ => return None,
            },
            Err(_) => return None,
        }
    }
    Some(out)
}

/// Reads a whole numeric array of any length.
async fn float_array<S: AsyncObjectSource>(src: &S, obj: Option<&Object>) -> Option<Vec<f32>> {
    let arr = match src.resolve(obj?).await {
        Ok(Object::Array(a)) => a,
        _ => return None,
    };
    let n = arr.len();
    floats(src, Some(&Object::Array(arr)), n).await
}

/// The dictionary of a function object, which is a plain dictionary for
/// types 2 and 3 and a stream for types 0 and 4.
async fn function_dict<S: AsyncObjectSource>(
    src: &S,
    obj: &Object,
) -> Option<(Dict, Option<Vec<u8>>)> {
    match src.resolve(obj).await.ok()? {
        Object::Dict(d) => Some((d, None)),
        Object::Stream(s) => {
            let data = decoded_stream_data_with(src, &s).await.ok()?;
            Some((s.dict.clone(), Some(data)))
        }
        _ => None,
    }
}

/// Loads `/Function` — one function or an array of them — into an arena.
/// `Err` is a structural failure worth reporting verbatim.
pub(crate) async fn load_functions<S: AsyncObjectSource>(
    src: &S,
    obj: &Object,
) -> Result<Functions, Error> {
    let mut queue: Vec<Object> = Vec::new();
    match src.resolve(obj).await {
        Ok(Object::Array(items)) => queue.extend(items.iter().cloned()),
        Ok(other) => queue.push(other),
        Err(e) => return Err(e),
    }
    let mut functions = Functions {
        nodes: Vec::new(),
        roots: Vec::new(),
    };
    // The queue holds (object, destination): a root, or a stitching child
    // slot that must be patched once the node index exists.
    let mut work: Vec<(Object, Option<(usize, usize)>)> =
        queue.into_iter().map(|o| (o, None)).collect();
    let mut cursor = 0;
    while cursor < work.len() {
        if functions.nodes.len() >= MAX_FUNCTIONS {
            return Err(Error::Other("shading function tree too large".into()));
        }
        let (obj, parent) = work[cursor].clone();
        cursor += 1;
        let Some((dict, data)) = function_dict(src, &obj).await else {
            return Err(Error::Other("shading function is not one".into()));
        };
        let kind = dict.get_int("FunctionType").unwrap_or(-1);
        let domain: [f32; 2] = match floats(src, dict.get("Domain"), 2).await {
            Some(d) => [d[0], d[1]],
            None => [0.0, 1.0],
        };
        let node = match kind {
            2 => {
                let c0 = float_array(src, dict.get("C0")).await.unwrap_or(vec![0.0]);
                let c1 = float_array(src, dict.get("C1")).await.unwrap_or(vec![1.0]);
                if c0.len() != c1.len() || c0.is_empty() {
                    return Err(Error::Other("exponential function C0/C1 disagree".into()));
                }
                let n = floats(src, dict.get("N"), 1)
                    .await
                    .map(|v| v[0])
                    .or_else(|| dict.get_int("N").map(|n| n as f32))
                    .unwrap_or(1.0);
                Node::Exponential { domain, c0, c1, n }
            }
            3 => {
                let subs = match src
                    .resolve(dict.get("Functions").ok_or_else(|| {
                        Error::Other("stitching function has no /Functions".into())
                    })?)
                    .await
                {
                    Ok(Object::Array(a)) => a,
                    _ => return Err(Error::Other("stitching /Functions is not an array".into())),
                };
                if subs.is_empty() {
                    return Err(Error::Other("stitching function has no parts".into()));
                }
                let bounds = float_array(src, dict.get("Bounds"))
                    .await
                    .unwrap_or_default();
                let encode = float_array(src, dict.get("Encode"))
                    .await
                    .unwrap_or_else(|| (0..subs.len()).flat_map(|_| [0.0, 1.0]).collect());
                if bounds.len() + 1 != subs.len() || encode.len() < 2 * subs.len() {
                    return Err(Error::Other("stitching bounds do not partition".into()));
                }
                let mut children = Vec::with_capacity(subs.len());
                let node_index = functions.nodes.len();
                for (slot, sub) in subs.iter().enumerate() {
                    children.push(usize::MAX); // patched when the child loads
                    work.push((sub.clone(), Some((node_index, slot))));
                }
                Node::Stitching {
                    domain,
                    children,
                    bounds,
                    encode,
                }
            }
            0 => {
                let data =
                    data.ok_or_else(|| Error::Other("sampled function carries no stream".into()))?;
                let size: Vec<usize> = match float_array(src, dict.get("Size")).await {
                    Some(s)
                        if (1..=MAX_COMPS).contains(&s.len())
                            && s.iter().all(|&v| (1.0..=MAX_SAMPLES as f32).contains(&v)) =>
                    {
                        s.iter().map(|&v| v as usize).collect()
                    }
                    _ => return Err(Error::Other("sampled function /Size unusable".into())),
                };
                let samples = size
                    .iter()
                    .try_fold(1u64, |p, &s| p.checked_mul(s as u64))
                    .filter(|&p| p <= MAX_SAMPLES);
                if samples.is_none() {
                    return Err(Error::Other("sampled function grid too large".into()));
                }
                let m = size.len();
                let domain = floats(src, dict.get("Domain"), 2 * m)
                    .await
                    .unwrap_or_else(|| (0..m).flat_map(|_| [0.0, 1.0]).collect());
                let bps = match dict.get_int("BitsPerSample") {
                    Some(b @ (1 | 2 | 4 | 8 | 12 | 16 | 24 | 32)) => b as u32,
                    _ => return Err(Error::Other("sampled function bits unusable".into())),
                };
                let range = float_array(src, dict.get("Range"))
                    .await
                    .filter(|r| r.len() >= 2 && r.len() % 2 == 0)
                    .ok_or_else(|| Error::Other("sampled function has no /Range".into()))?;
                let outputs = range.len() / 2;
                let encode = floats(src, dict.get("Encode"), 2 * m)
                    .await
                    .unwrap_or_else(|| size.iter().flat_map(|&s| [0.0, (s - 1) as f32]).collect());
                let decode = float_array(src, dict.get("Decode"))
                    .await
                    .filter(|d| d.len() == range.len())
                    .unwrap_or(range);
                Node::Sampled {
                    domain,
                    encode,
                    decode,
                    size,
                    bps,
                    outputs,
                    data,
                }
            }
            4 => {
                let data = data
                    .ok_or_else(|| Error::Other("calculator function carries no stream".into()))?;
                let domain = float_array(src, dict.get("Domain"))
                    .await
                    .filter(|d| d.len() >= 2 && d.len() % 2 == 0 && d.len() / 2 <= MAX_COMPS)
                    .ok_or_else(|| Error::Other("calculator function /Domain unusable".into()))?;
                let range = float_array(src, dict.get("Range"))
                    .await
                    .filter(|r| r.len() >= 2 && r.len() % 2 == 0)
                    .ok_or_else(|| Error::Other("calculator function has no /Range".into()))?;
                let items = parse_calculator(&data)?;
                let mut program = Vec::new();
                compile_calculator(&items, &mut program)?;
                Node::Calculator {
                    domain,
                    range,
                    program,
                }
            }
            _ => return Err(Error::Other("unknown function type".into())),
        };
        let index = functions.nodes.len();
        functions.nodes.push(node);
        match parent {
            None => functions.roots.push(index),
            Some((parent_index, slot)) => {
                if let Node::Stitching { children, .. } = &mut functions.nodes[parent_index] {
                    children[slot] = index;
                }
            }
        }
    }
    // A stitching child that never loaded leaves usize::MAX behind; the cap
    // above is the only way to get here, and it already errored.
    Ok(functions)
}

impl Shading {
    /// Loads a shading dictionary (or stream — mesh shadings are streams,
    /// and they answer `Ok(None)`). `Ok(None)` = a shading type this
    /// renderer does not paint, for the caller to report as unsupported;
    /// `Err` = a structural failure, reported verbatim.
    pub(crate) async fn load_with<S: AsyncObjectSource>(
        src: &S,
        obj: &Object,
    ) -> Result<Option<Shading>, Error> {
        let dict = match src.resolve(obj).await? {
            Object::Dict(d) => d,
            Object::Stream(s) => s.dict.clone(),
            _ => return Err(Error::Other("shading is not a dictionary".into())),
        };
        let kind = dict.get_int("ShadingType").unwrap_or(-1);
        let geometry = match kind {
            2 => {
                let c = floats(src, dict.get("Coords"), 4)
                    .await
                    .ok_or_else(|| Error::Other("axial shading /Coords unusable".into()))?;
                Geometry::Axial {
                    p0: Point { x: c[0], y: c[1] },
                    p1: Point { x: c[2], y: c[3] },
                }
            }
            3 => {
                let c = floats(src, dict.get("Coords"), 6)
                    .await
                    .ok_or_else(|| Error::Other("radial shading /Coords unusable".into()))?;
                if c[2] < 0.0 || c[5] < 0.0 {
                    return Err(Error::Other("radial shading radius negative".into()));
                }
                Geometry::Radial {
                    c0: Point { x: c[0], y: c[1] },
                    r0: c[2],
                    c1: Point { x: c[3], y: c[4] },
                    r1: c[5],
                }
            }
            1 | 4..=7 => return Ok(None),
            _ => return Err(Error::Other("unknown shading type".into())),
        };
        let cs_obj = dict
            .get("ColorSpace")
            .ok_or_else(|| Error::Other("shading has no /ColorSpace".into()))?;
        let cs = ColorSpace::parse_with(src, cs_obj).await;
        let functions = match dict.get("Function") {
            Some(f) => load_functions(src, f).await?,
            None => return Err(Error::Other("shading has no /Function".into())),
        };
        let domain = match floats(src, dict.get("Domain"), 2).await {
            Some(d) => [d[0], d[1]],
            None => [0.0, 1.0],
        };
        let extend = match src
            .resolve(dict.get("Extend").unwrap_or(&Object::Null))
            .await
        {
            Ok(Object::Array(a)) if a.len() >= 2 => [
                matches!(a[0], Object::Bool(true)),
                matches!(a[1], Object::Bool(true)),
            ],
            _ => [false, false],
        };
        let bbox = floats(src, dict.get("BBox"), 4)
            .await
            .map(|b| [b[0], b[1], b[2], b[3]]);
        let background = float_array(src, dict.get("Background"))
            .await
            .map(|comps| cs.to_rgb(&comps));
        Ok(Some(Shading {
            geometry,
            cs,
            functions,
            domain,
            extend,
            bbox,
            background,
        }))
    }

    /// Paints the shading over every pixel `region` covers (the whole page
    /// when `None`), compositing at `alpha` × coverage under `blend`.
    /// `to_device` maps the shading's target space to device pixels; a
    /// singular matrix paints nothing (the caller reports it).
    pub(crate) fn paint(
        &self,
        pix: &mut Pixmap,
        region: Option<&Mask>,
        alpha: f32,
        to_device: Matrix,
        blend: BlendMode,
    ) {
        let Some(inv) = to_device.invert() else {
            return;
        };
        let Some(alpha) = clamped_alpha(alpha) else {
            return;
        };
        let (x_lo, x_hi, y_lo, y_hi) = region_bounds(pix, region);
        let normal = blend == BlendMode::Normal;
        let mut comps = [0f32; MAX_COMPS];
        for y in y_lo..y_hi {
            for x in x_lo..x_hi {
                let cov = region.map_or(255, |m| m.coverage(x, y));
                if cov == 0 {
                    continue;
                }
                let p = inv.apply(Point {
                    x: x as f32 + 0.5,
                    y: y as f32 + 0.5,
                });
                let Some(s) = self.geometry.param_at(p, self.extend) else {
                    continue;
                };
                let t = self.domain[0] + (self.domain[1] - self.domain[0]) * s;
                let n = self.functions.eval(&[t], &mut comps);
                let rgb8 = quantize_rgb(self.cs.to_rgb(&comps[..n]));
                let a = alpha * cov as f32 / 255.0;
                shade_pixel(pix, x, y, a, rgb8, normal, blend);
            }
        }
    }

    /// Composites `/Background` over every pixel `region` covers, at
    /// `alpha` × coverage under `blend` — the first half of the spec's
    /// paint-twice model (Table 78: background first, then the shading
    /// over it), sharing the shading's own clipping including `/BBox`.
    pub(crate) fn paint_background(
        &self,
        pix: &mut Pixmap,
        region: &Mask,
        alpha: f32,
        blend: BlendMode,
    ) {
        let Some(bg) = self.background else {
            return;
        };
        let Some(alpha) = clamped_alpha(alpha) else {
            return;
        };
        let (x_lo, x_hi, y_lo, y_hi) = region_bounds(pix, Some(region));
        let normal = blend == BlendMode::Normal;
        let rgb8 = quantize_rgb(bg);
        for y in y_lo..y_hi {
            for x in x_lo..x_hi {
                let cov = region.coverage(x, y);
                if cov == 0 {
                    continue;
                }
                let a = alpha * cov as f32 / 255.0;
                shade_pixel(pix, x, y, a, rgb8, normal, blend);
            }
        }
    }
}

/// A constant alpha normalized for painting: clamped to 0..=1 (non-finite
/// reads as opaque), `None` when nothing would paint.
fn clamped_alpha(alpha: f32) -> Option<f32> {
    let alpha = if alpha.is_finite() {
        alpha.clamp(0.0, 1.0)
    } else {
        1.0
    };
    (alpha > 0.0).then_some(alpha)
}

/// The device-pixel rectangle a paint loop walks: the region's bbox clamped
/// to the page, or the whole page without a region.
fn region_bounds(pix: &Pixmap, region: Option<&Mask>) -> (u32, u32, u32, u32) {
    match region {
        Some(m) => (
            m.x0,
            (m.x0 + m.bbox_w).min(pix.width),
            m.y0,
            (m.y0 + m.bbox_h).min(pix.height),
        ),
        None => (0, pix.width, 0, pix.height),
    }
}

/// Unit-range RGB to RGB8.
fn quantize_rgb(rgb: [f32; 3]) -> [u8; 3] {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    [q(rgb[0]), q(rgb[1]), q(rgb[2])]
}

/// Composites one shading pixel through the raster blend seam.
#[inline]
fn shade_pixel(
    pix: &mut Pixmap,
    x: u32,
    y: u32,
    a: f32,
    rgb8: [u8; 3],
    normal: bool,
    blend: BlendMode,
) {
    let opaque = [rgb8[0], rgb8[1], rgb8[2], 255];
    let off = ((y * pix.width + x) * 4) as usize;
    let dst = &mut pix.data[off..off + 4];
    if normal {
        paint_pixel::<true>(dst, a, rgb8, opaque, blend);
    } else {
        paint_pixel::<false>(dst, a, rgb8, opaque, blend);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::parser::{NoResolve, Parser};
    use pdfboss_core::{block_on, Document, Immediate};
    use pdfboss_testkit::PdfBuilder;

    fn obj(src: &[u8]) -> Object {
        Parser::new(src).parse_object(&NoResolve).unwrap()
    }

    fn load(dict: &str, data: &[u8]) -> Result<Functions, Error> {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        b.stream(5, dict, data);
        let doc = Document::load(b.build(1)).unwrap();
        block_on(load_functions(&Immediate(&doc), &obj(b"5 0 R")))
    }

    fn eval(f: &Functions, inputs: &[f32]) -> Vec<f32> {
        let mut out = [0f32; MAX_COMPS];
        let n = f.eval(inputs, &mut out);
        out[..n].to_vec()
    }

    fn close(got: &[f32], want: &[f32]) {
        assert_eq!(got.len(), want.len(), "{got:?} vs {want:?}");
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() < 1e-4, "{got:?} vs {want:?}");
        }
    }

    /// A 2x2 sample grid interpolated per ISO 32000-2 7.10.2: the first
    /// dimension varies fastest, so the four bytes are f(0,0), f(1,0),
    /// f(0,1), f(1,1).
    #[test]
    fn a_two_input_sampled_grid_interpolates_multilinearly() {
        let f = load(
            "/FunctionType 0 /Domain [0 1 0 1] /Range [0 1] /Size [2 2] /BitsPerSample 8",
            &[0, 100, 200, 255],
        )
        .unwrap();
        close(&eval(&f, &[0.0, 0.0]), &[0.0]);
        close(&eval(&f, &[1.0, 0.0]), &[100.0 / 255.0]);
        close(&eval(&f, &[0.0, 1.0]), &[200.0 / 255.0]);
        close(&eval(&f, &[1.0, 1.0]), &[1.0]);
        // Center: the plain average of all four corners.
        close(&eval(&f, &[0.5, 0.5]), &[138.75 / 255.0]);
        // Hand-computed bilinear blend at (0.25, 0.75):
        // 0.1875*0 + 0.0625*100 + 0.5625*200 + 0.1875*255 = 166.5625.
        close(&eval(&f, &[0.25, 0.75]), &[166.5625 / 255.0]);
        // Out-of-domain inputs clamp per dimension.
        close(&eval(&f, &[-2.0, 7.0]), &[200.0 / 255.0]);
    }

    #[test]
    fn a_one_input_sampled_function_still_interpolates_linearly() {
        let f = load(
            "/FunctionType 0 /Domain [0 1] /Range [0 1] /Size [4] /BitsPerSample 8",
            &[0, 85, 170, 255],
        )
        .unwrap();
        close(&eval(&f, &[0.0]), &[0.0]);
        close(&eval(&f, &[0.5]), &[127.5 / 255.0]);
        close(&eval(&f, &[1.0]), &[1.0]);
    }

    /// A missing multi-input /Encode defaults to [0 size_i-1] per dimension,
    /// and /Decode maps samples into each output's range.
    #[test]
    fn multi_input_encode_and_decode_defaults_apply_per_dimension() {
        let f = load(
            "/FunctionType 0 /Domain [0 1 0 1] /Range [0 1] /Size [3 2] \
             /BitsPerSample 8 /Decode [1 0]",
            &[0, 51, 102, 153, 204, 255],
        )
        .unwrap();
        // (1, 0) encodes to grid point (2, 0): sample 102, decoded 1 - s.
        close(&eval(&f, &[1.0, 0.0]), &[1.0 - 102.0 / 255.0]);
        // (0.5, 1) encodes to (1, 1): sample index 1 + 2*3 = 204.
        close(&eval(&f, &[0.5, 1.0]), &[1.0 - 204.0 / 255.0]);
    }

    #[test]
    fn exponential_and_stitching_read_only_the_first_input() {
        let f = load(
            "/FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1",
            b"unused",
        );
        // Type 2 is a dictionary in real files; the stream dict works too.
        let f = f.unwrap();
        close(&eval(&f, &[0.25, 9.0]), &[0.25]);
        close(&eval(&f, &[]), &[0.0]);
    }

    fn calc(domain: &str, range: &str, program: &str) -> Functions {
        load(
            &format!("/FunctionType 4 /Domain [{domain}] /Range [{range}]"),
            program.as_bytes(),
        )
        .unwrap()
    }

    #[test]
    fn calculator_arithmetic_operators_compute() {
        let f = calc(
            "0 1",
            "0 2000 0 2000 0 2000 0 2000 0 2000 0 2000",
            "{ 3 4 add 2 mul 2 10 exp 100 log 9 sqrt 7 2 div 1 ln }",
        );
        close(&eval(&f, &[0.0]), &[14.0, 1024.0, 2.0, 3.0, 3.5, 0.0]);
    }

    #[test]
    fn calculator_atan_returns_degrees_in_every_quadrant() {
        let f = calc(
            "0 1",
            "0 360 0 360 0 360 0 360 0 360",
            "{ 1 1 atan 1 -1 atan -1 -1 atan -1 1 atan 1 0 atan }",
        );
        close(&eval(&f, &[0.0]), &[45.0, 135.0, 225.0, 315.0, 90.0]);
    }

    #[test]
    fn calculator_integer_division_and_modulo_keep_the_dividend_sign() {
        let f = calc(
            "0 1",
            "-10 10 -10 10 -10 10 -10 10",
            "{ -7 2 idiv -7 3 mod 7 -3 mod 6 2 idiv }",
        );
        close(&eval(&f, &[0.0]), &[-3.0, -1.0, 1.0, 3.0]);
    }

    #[test]
    fn calculator_roll_rotates_both_directions() {
        let up = calc("0 1", "0 5 0 5 0 5", "{ 1 2 3 3 1 roll }");
        close(&eval(&up, &[0.0]), &[3.0, 1.0, 2.0]);
        let down = calc("0 1", "0 5 0 5 0 5", "{ 1 2 3 3 -1 roll }");
        close(&eval(&down, &[0.0]), &[2.0, 3.0, 1.0]);
    }

    #[test]
    fn calculator_stack_operators_manage_the_stack() {
        let index = calc("0 1", "0 5 0 5 0 5 0 5", "{ 1 2 3 2 index }");
        close(&eval(&index, &[0.0]), &[1.0, 2.0, 3.0, 1.0]);
        let copy = calc("0 1", "0 5 0 5 0 5 0 5", "{ 1 2 2 copy }");
        close(&eval(&copy, &[0.0]), &[1.0, 2.0, 1.0, 2.0]);
        let mix = calc("0 1", "0 1", "{ dup 1 exch sub exch pop }");
        close(&eval(&mix, &[0.3]), &[0.7]);
    }

    #[test]
    fn calculator_nested_conditionals_branch() {
        let f = calc(
            "0 1",
            "0 1",
            "{ dup 0.25 lt { pop 0 } { 0.75 lt { 0.5 } { 1 } ifelse } ifelse }",
        );
        close(&eval(&f, &[0.1]), &[0.0]);
        close(&eval(&f, &[0.5]), &[0.5]);
        close(&eval(&f, &[0.9]), &[1.0]);
    }

    #[test]
    fn calculator_logic_operators_split_on_operand_type() {
        let ints = calc(
            "0 1",
            "-20 20 -20 20 -20 20 -20 20",
            "{ 12 10 and 12 10 or 12 10 xor 7 not }",
        );
        close(&eval(&ints, &[0.0]), &[8.0, 14.0, 6.0, -8.0]);
        let bools = calc(
            "0 1",
            "0 1 0 1",
            "{ true false or { 1 } { 0 } ifelse true not { 1 } { 0 } ifelse }",
        );
        close(&eval(&bools, &[0.0]), &[1.0, 0.0]);
    }

    #[test]
    fn calculator_bitshift_shifts_both_directions() {
        let f = calc("0 1", "0 10 0 10", "{ 1 3 bitshift 8 -2 bitshift }");
        close(&eval(&f, &[0.0]), &[8.0, 2.0]);
    }

    #[test]
    fn calculator_comparisons_and_mixed_type_equality() {
        let f = calc(
            "0 1",
            "0 1 0 1 0 1 0 1",
            "{ 3 4 lt { 1 } { 0 } ifelse 3 3 ge { 1 } { 0 } ifelse \
              1 true eq { 1 } { 0 } ifelse 1 true ne { 1 } { 0 } ifelse }",
        );
        close(&eval(&f, &[0.0]), &[1.0, 1.0, 0.0, 1.0]);
    }

    /// `round` resolves ties toward the greater integer, unlike a
    /// round-half-away-from-zero.
    #[test]
    fn calculator_rounding_family() {
        let f = calc(
            "0 1",
            "-10 10 -10 10 -10 10 -10 10 -10 10 -10 10",
            "{ 2.5 round -2.5 round -3.7 truncate -3.7 floor 3.2 ceiling -3.7 cvi }",
        );
        close(&eval(&f, &[0.0]), &[3.0, -2.0, -3.0, -4.0, 4.0, -3.0]);
    }

    #[test]
    fn calculator_inputs_arrive_in_order_and_clamp_to_domain() {
        let f = calc("0 1 0 1", "-1 1", "{ sub }");
        close(&eval(&f, &[0.7, 0.2]), &[0.5]);
        close(&eval(&f, &[2.0, -1.0]), &[1.0]);
    }

    #[test]
    fn calculator_runtime_failures_clamp_to_the_range_floor() {
        // Stack underflow: `add` finds one operand, not two.
        let underflow = calc("0 1", "5 10", "{ add }");
        close(&eval(&underflow, &[0.5]), &[5.0]);
        let zero_div = calc("0 1", "0 1", "{ pop 1 0 div }");
        close(&eval(&zero_div, &[0.5]), &[0.0]);
        // Bitwise `and` over a boolean and a number is a type error.
        let mixed = calc("0 1", "0 1", "{ pop true 1 and }");
        close(&eval(&mixed, &[0.5]), &[0.0]);
        // A boolean is not a number, so it cannot be an output.
        let boolean_out = calc("0 1", "0 1", "{ pop true }");
        close(&eval(&boolean_out, &[0.5]), &[0.0]);
    }

    #[test]
    fn calculator_success_outputs_clamp_to_range() {
        let f = calc("0 1", "0 1 0 1", "{ pop 2 -1 }");
        close(&eval(&f, &[0.5]), &[1.0, 0.0]);
    }

    #[test]
    fn a_runaway_calculator_program_clamps_instead_of_hanging() {
        let mut runaway = String::from("{ 0 ");
        for _ in 0..8000 {
            runaway.push_str("1 add ");
        }
        runaway.push('}');
        let f = calc("0 1", "0 20000", &runaway);
        close(&eval(&f, &[0.0]), &[0.0]);
        // The same shape under the step budget still computes.
        let mut fine = String::from("{ 0 ");
        for _ in 0..100 {
            fine.push_str("1 add ");
        }
        fine.push('}');
        let f = calc("0 1", "0 20000", &fine);
        close(&eval(&f, &[0.0]), &[100.0]);
    }

    #[test]
    fn calculator_comments_and_whitespace_are_skipped() {
        let f = calc("0 1", "0 10", "{ % a comment\n 3 4 add pop 5 }");
        close(&eval(&f, &[0.0]), &[5.0]);
    }

    #[test]
    fn malformed_calculator_programs_fail_at_load() {
        let check = |program: &str| {
            let loaded = load(
                "/FunctionType 4 /Domain [0 1] /Range [0 1]",
                program.as_bytes(),
            );
            assert!(loaded.is_err(), "{program:?} should not load");
        };
        check("{ 1 2 add");
        check("1 2 add }");
        check("{ 1 2 frobnicate }");
        check("{ { 1 } }");
        check("{ 1 if }");
        check("{ { 1 } { 2 } if }");
        check("{ 1 2 } }");
        check("{ 1 } 2");
        check("");
    }

    #[test]
    fn a_calculator_function_without_range_fails_at_load() {
        assert!(load("/FunctionType 4 /Domain [0 1]", b"{ 1 }").is_err());
    }

    #[test]
    fn oversized_sample_grids_are_refused() {
        // Nine input dimensions overflow every consumer (MAX_COMPS is 8).
        let nine = load(
            "/FunctionType 0 /Domain [0 1 0 1 0 1 0 1 0 1 0 1 0 1 0 1 0 1] /Range [0 1] \
             /Size [2 2 2 2 2 2 2 2 2] /BitsPerSample 8",
            &[0; 512],
        );
        assert!(nine.is_err());
        // A sample count beyond the cap is hostile, not a gradient.
        let huge = load(
            "/FunctionType 0 /Domain [0 1 0 1] /Range [0 1] /Size [100000 100000] \
             /BitsPerSample 8",
            &[0; 4],
        );
        assert!(huge.is_err());
    }
}
