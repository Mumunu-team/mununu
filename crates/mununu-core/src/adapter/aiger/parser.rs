//! AIGER ASCII (.aag) format parser.
//!
//! Parses the AAG (ASCII AIGER) format as defined in the AIGER 1.9 specification.
//! Binary .aig format is not yet supported.

use super::ast::*;
use crate::adapter::{AdapterError, AdapterErrorKind, SourceLocation};

/// Parse an AAG (ASCII AIGER) file.
pub fn parse(content: &str) -> Result<Circuit, AdapterError> {
    let mut lines = content.lines().enumerate();

    // Parse header: aag M I L O A [B [C [J [F]]]]
    let (line_num, header_line) = lines
        .next()
        .ok_or_else(|| err(0, "empty file (expected 'aag' header line)"))?;

    let parts: Vec<&str> = header_line.split_whitespace().collect();
    if parts.is_empty() || parts[0] != "aag" {
        return Err(err(
            line_num,
            &format!(
                "expected 'aag' header but found '{}' (binary .aig format is not supported)",
                parts[0]
            ),
        ));
    }
    if parts.len() < 6 {
        return Err(err(
            line_num,
            &format!(
                "incomplete header: expected 'aag M I L O A [B [C [J [F]]]]' but found {} field(s)",
                parts.len()
            ),
        ));
    }

    let max_var = parse_usize(parts[1], line_num, "M")?;
    let num_inputs = parse_usize(parts[2], line_num, "I")?;
    let num_latches = parse_usize(parts[3], line_num, "L")?;
    let num_outputs = parse_usize(parts[4], line_num, "O")?;
    let num_gates = parse_usize(parts[5], line_num, "A")?;
    let num_bad = if parts.len() > 6 {
        parse_usize(parts[6], line_num, "B")?
    } else {
        0
    };
    let num_constraints = if parts.len() > 7 {
        parse_usize(parts[7], line_num, "C")?
    } else {
        0
    };
    let num_justice = if parts.len() > 8 {
        parse_usize(parts[8], line_num, "justice count (J)")?
    } else {
        0
    };
    let num_fairness = if parts.len() > 9 {
        parse_usize(parts[9], line_num, "fairness count (F)")?
    } else {
        0
    };

    // Parse inputs (one literal per line)
    let mut inputs = Vec::with_capacity(num_inputs);
    for _ in 0..num_inputs {
        let (ln, line) = next_content_line(&mut lines)?;
        let lit = parse_usize(line.trim(), ln, "input literal")?;
        inputs.push(lit);
    }

    // Parse latches (current next [init])
    let mut latches = Vec::with_capacity(num_latches);
    for _ in 0..num_latches {
        let (ln, line) = next_content_line(&mut lines)?;
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            return Err(err(
                ln,
                &format!(
                    "latch definition requires at least 2 fields (current next [init]) but found {}",
                    parts.len()
                ),
            ));
        }
        let current = parse_usize(parts[0], ln, "latch current")?;
        let next = parse_usize(parts[1], ln, "latch next")?;
        let init = if parts.len() > 2 {
            latch_init(parse_usize(parts[2], ln, "latch init")?, current)
        } else {
            0
        };
        latches.push(Latch {
            current,
            next,
            init,
        });
    }

    // Parse outputs (one literal per line) — kept for the BTOR2 lowering; the CLTS path uses
    // the bad outputs.
    let mut outputs = Vec::with_capacity(num_outputs);
    for _ in 0..num_outputs {
        let (ln, line) = next_content_line(&mut lines)?;
        let lit = parse_usize(line.trim(), ln, "output literal")?;
        outputs.push(lit);
    }

    // Parse bad outputs
    let mut bad_outputs = Vec::with_capacity(num_bad);
    for _ in 0..num_bad {
        let (ln, line) = next_content_line(&mut lines)?;
        let lit = parse_usize(line.trim(), ln, "bad literal")?;
        bad_outputs.push(lit);
    }

    // Parse constraints
    let mut constraints = Vec::with_capacity(num_constraints);
    for _ in 0..num_constraints {
        let (ln, line) = next_content_line(&mut lines)?;
        let lit = parse_usize(line.trim(), ln, "constraint literal")?;
        constraints.push(lit);
    }

    // Parse justice sets
    let mut justice_sets = Vec::with_capacity(num_justice);
    for _ in 0..num_justice {
        let (ln, line) = next_content_line(&mut lines)?;
        let size = parse_usize(line.trim(), ln, "justice size")?;
        let mut set = Vec::with_capacity(size);
        for _ in 0..size {
            let (ln2, line2) = next_content_line(&mut lines)?;
            let lit = parse_usize(line2.trim(), ln2, "justice literal")?;
            set.push(lit);
        }
        justice_sets.push(set);
    }

    // Parse fairness constraints (one literal per line)
    let mut fairness = Vec::with_capacity(num_fairness);
    for _ in 0..num_fairness {
        let (ln, line) = next_content_line(&mut lines)?;
        let lit = parse_usize(line.trim(), ln, "fairness literal")?;
        fairness.push(lit);
    }

    // Parse AND gates (lhs rhs0 rhs1)
    let mut gates = Vec::with_capacity(num_gates);
    for _ in 0..num_gates {
        let (ln, line) = next_content_line(&mut lines)?;
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            return Err(err(
                ln,
                &format!(
                    "AND gate requires 3 fields (lhs rhs0 rhs1) but found {}",
                    parts.len()
                ),
            ));
        }
        let lhs = parse_usize(parts[0], ln, "gate lhs")?;
        let rhs0 = parse_usize(parts[1], ln, "gate rhs0")?;
        let rhs1 = parse_usize(parts[2], ln, "gate rhs1")?;
        gates.push(Gate { lhs, rhs0, rhs1 });
    }

    // Parse symbol table and comments
    let mut symbols = SymbolTable {
        input_names: vec![None; num_inputs],
        latch_names: vec![None; num_latches],
        output_names: vec![None; num_outputs],
        bad_names: vec![None; num_bad],
        constraint_names: vec![None; num_constraints],
    };

    for (_ln, line) in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('c') {
            break; // comment section or end
        }
        // Symbol: i<idx> name, l<idx> name, o<idx> name, b<idx> name
        if let Some(rest) = line.strip_prefix('i')
            && let Some((idx_str, name)) = rest.split_once(' ')
            && let Ok(idx) = idx_str.parse::<usize>()
            && idx < symbols.input_names.len()
        {
            symbols.input_names[idx] = Some(name.to_string());
        } else if let Some(rest) = line.strip_prefix('l')
            && let Some((idx_str, name)) = rest.split_once(' ')
            && let Ok(idx) = idx_str.parse::<usize>()
            && idx < symbols.latch_names.len()
        {
            symbols.latch_names[idx] = Some(name.to_string());
        } else if let Some(rest) = line.strip_prefix('b')
            && let Some((idx_str, name)) = rest.split_once(' ')
            && let Ok(idx) = idx_str.parse::<usize>()
            && idx < symbols.bad_names.len()
        {
            symbols.bad_names[idx] = Some(name.to_string());
        }
    }

    Ok(Circuit {
        max_var,
        inputs,
        latches,
        gates,
        outputs,
        bad_outputs,
        constraints,
        justice_sets,
        fairness,
        symbols,
    })
}

/// AIGER 1.9 latch init field: `0` / `1`, or the latch's own literal = undefined (2).
fn latch_init(field: usize, current: Literal) -> u8 {
    match field {
        0 => 0,
        1 => 1,
        f if f == current => 2,
        _ => 2,
    }
}

/// Parse an AIGER file in EITHER format (`aag` ASCII or `aig` binary) from raw bytes — the
/// HWMCC sets ship binary 1.9 files (`.aig`), which are not valid UTF-8 past the header.
pub fn parse_bytes(bytes: &[u8]) -> Result<Circuit, AdapterError> {
    if bytes.starts_with(b"aig ") {
        parse_binary(bytes)
    } else {
        let text = std::str::from_utf8(bytes)
            .map_err(|e| err(0, &format!("not UTF-8 ASCII AIGER: {e}")))?;
        parse(text)
    }
}

/// The binary AIGER 1.9 format: an ASCII header `aig M I L O A [B C J F]`, then ASCII lines for
/// the latches (`next [init]`), outputs, bad, constraints, justice (sizes, then the literals) and
/// fairness, then `A` AND gates as two delta-coded varints each (`lhs − rhs0`, `rhs0 − rhs1`, the
/// `i`-th gate's lhs being `2·(I + L + i + 1)`), then an optional symbol table and comments.
fn parse_binary(bytes: &[u8]) -> Result<Circuit, AdapterError> {
    let mut pos = 0usize;
    let mut line_no = 0usize;
    let next_line = |pos: &mut usize, line_no: &mut usize| -> Result<String, AdapterError> {
        let start = *pos;
        while *pos < bytes.len() && bytes[*pos] != b'\n' {
            *pos += 1;
        }
        if *pos >= bytes.len() && start == *pos {
            return Err(err(
                *line_no,
                "unexpected end of file in the binary AIGER header section",
            ));
        }
        let line = String::from_utf8_lossy(&bytes[start..*pos]).to_string();
        *pos += 1; // the newline
        *line_no += 1;
        Ok(line)
    };
    let header = next_line(&mut pos, &mut line_no)?;
    let parts: Vec<&str> = header.split_whitespace().collect();
    if parts.first() != Some(&"aig") || parts.len() < 6 {
        return Err(err(0, "expected an `aig M I L O A [B C J F]` header"));
    }
    let n = |i: usize, what: &str| -> Result<usize, AdapterError> {
        parts.get(i).map_or(Ok(0), |s| parse_usize(s, 0, what))
    };
    let (max_var, num_inputs, num_latches, num_outputs, num_gates) =
        (n(1, "M")?, n(2, "I")?, n(3, "L")?, n(4, "O")?, n(5, "A")?);
    let (num_bad, num_constraints, num_justice, num_fairness) =
        (n(6, "B")?, n(7, "C")?, n(8, "J")?, n(9, "F")?);
    if max_var != num_inputs + num_latches + num_gates {
        return Err(err(
            0,
            &format!(
                "binary AIGER header: M = {max_var} but I + L + A = {}",
                num_inputs + num_latches + num_gates
            ),
        ));
    }
    // Inputs are implicit: literals 2, 4, …, 2I.
    let inputs: Vec<Literal> = (1..=num_inputs).map(|v| 2 * v).collect();
    let mut latches = Vec::with_capacity(num_latches);
    for k in 0..num_latches {
        let line = next_line(&mut pos, &mut line_no)?;
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.is_empty() {
            return Err(err(line_no, "latch line needs `next [init]`"));
        }
        let current = 2 * (num_inputs + k + 1);
        let next = parse_usize(f[0], line_no, "latch next")?;
        let init = if f.len() > 1 {
            latch_init(parse_usize(f[1], line_no, "latch init")?, current)
        } else {
            0
        };
        latches.push(Latch {
            current,
            next,
            init,
        });
    }
    let read_lits = |count: usize,
                     what: &str,
                     pos: &mut usize,
                     line_no: &mut usize|
     -> Result<Vec<Literal>, AdapterError> {
        let mut v = Vec::with_capacity(count);
        for _ in 0..count {
            let line = next_line(pos, line_no)?;
            v.push(parse_usize(line.trim(), *line_no, what)?);
        }
        Ok(v)
    };
    let outputs = read_lits(num_outputs, "output literal", &mut pos, &mut line_no)?;
    let bad_outputs = read_lits(num_bad, "bad literal", &mut pos, &mut line_no)?;
    let constraints = read_lits(
        num_constraints,
        "constraint literal",
        &mut pos,
        &mut line_no,
    )?;
    let sizes = read_lits(num_justice, "justice size", &mut pos, &mut line_no)?;
    let mut justice_sets = Vec::with_capacity(num_justice);
    for size in sizes {
        justice_sets.push(read_lits(size, "justice literal", &mut pos, &mut line_no)?);
    }
    let fairness = read_lits(num_fairness, "fairness literal", &mut pos, &mut line_no)?;
    // The binary AND section.
    let mut gates = Vec::with_capacity(num_gates);
    for i in 0..num_gates {
        let lhs = 2 * (num_inputs + num_latches + i + 1);
        let d0 = read_varint(bytes, &mut pos)
            .ok_or_else(|| err(line_no, "truncated binary AND section (delta 0)"))?;
        let d1 = read_varint(bytes, &mut pos)
            .ok_or_else(|| err(line_no, "truncated binary AND section (delta 1)"))?;
        let rhs0 = lhs
            .checked_sub(d0)
            .ok_or_else(|| err(line_no, "binary AND delta 0 exceeds lhs"))?;
        let rhs1 = rhs0
            .checked_sub(d1)
            .ok_or_else(|| err(line_no, "binary AND delta 1 exceeds rhs0"))?;
        gates.push(Gate { lhs, rhs0, rhs1 });
    }
    // Symbol table (`i<k> name`, `l<k> name`, `o<k> name`, `b<k>`, `c<k>`, `j<k>`, `f<k>`) and comments.
    let mut symbols = SymbolTable {
        input_names: vec![None; num_inputs],
        latch_names: vec![None; num_latches],
        output_names: vec![None; num_outputs],
        bad_names: vec![None; num_bad],
        constraint_names: vec![None; num_constraints],
    };
    while pos < bytes.len() {
        let line = match next_line(&mut pos, &mut line_no) {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() || line == "c" || line.starts_with("c ") {
            break;
        }
        let Some((tag, name)) = line.split_once(' ') else {
            continue;
        };
        let (kind, idx) = tag.split_at(1);
        let Ok(idx) = idx.parse::<usize>() else {
            continue;
        };
        let slot = match kind {
            "i" => symbols.input_names.get_mut(idx),
            "l" => symbols.latch_names.get_mut(idx),
            "o" => symbols.output_names.get_mut(idx),
            "b" => symbols.bad_names.get_mut(idx),
            "c" => symbols.constraint_names.get_mut(idx),
            _ => None,
        };
        if let Some(s) = slot {
            *s = Some(name.to_string());
        }
    }
    Ok(Circuit {
        max_var,
        inputs,
        latches,
        gates,
        outputs,
        bad_outputs,
        constraints,
        justice_sets,
        fairness,
        symbols,
    })
}

/// AIGER's LEB128-style varint: 7 bits per byte, little-endian, high bit = continuation.
fn read_varint(bytes: &[u8], pos: &mut usize) -> Option<usize> {
    let mut value: usize = 0;
    let mut shift = 0u32;
    loop {
        let b = *bytes.get(*pos)?;
        *pos += 1;
        value |= ((b & 0x7f) as usize) << shift;
        if b & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

fn next_content_line<'a>(
    lines: &mut impl Iterator<Item = (usize, &'a str)>,
) -> Result<(usize, &'a str), AdapterError> {
    lines.next().ok_or_else(|| {
        err(
            0,
            "unexpected end of file (more lines expected per header counts)",
        )
    })
}

fn parse_usize(s: &str, line: usize, context: &str) -> Result<usize, AdapterError> {
    s.parse().map_err(|_| {
        err(
            line,
            &format!("invalid {context} '{s}' (expected unsigned integer)"),
        )
    })
}

/// Create a parse error at the given line.
///
/// Column is always 1 because the AIGER format is line-oriented;
/// each line contains a single declaration or definition.
fn err(line: usize, msg: &str) -> AdapterError {
    AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: msg.to_string(),
        location: Some(SourceLocation {
            line: line + 1,
            column: 1,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_alarm_circuit() {
        // Alarm circuit from the adapter spec
        let input = "\
aag 3 1 1 0 1 1
2
4 7
4
6 3 5
i0 sensor
l0 alarm
b0 alarm_on
";
        let circuit = parse(input).unwrap();
        assert_eq!(circuit.num_inputs(), 1);
        assert_eq!(circuit.num_latches(), 1);
        assert_eq!(circuit.gates.len(), 1);
        assert_eq!(circuit.bad_outputs.len(), 1);
        assert_eq!(circuit.bad_outputs[0], 4); // alarm literal
        assert_eq!(circuit.input_name(0), "sensor");
        assert_eq!(circuit.latch_name(0), "alarm");
    }

    #[test]
    fn eval_alarm_circuit() {
        let input = "\
aag 3 1 1 0 1 1
2
4 7
4
6 3 5
i0 sensor
l0 alarm
b0 alarm_on
";
        let circuit = parse(input).unwrap();

        // State: alarm=0, Input: sensor=0 → next_alarm = OR(0,0) = 0
        let mut values = vec![false, false, false]; // [unused, sensor=0, alarm=0]
        circuit.eval_gates(&mut values);
        let next = circuit.next_state(&values);
        assert_eq!(next, vec![false]); // alarm stays 0

        // State: alarm=0, Input: sensor=1 → next_alarm = OR(1,0) = 1
        let mut values = vec![false, true, false]; // [unused, sensor=1, alarm=0]
        circuit.eval_gates(&mut values);
        let next = circuit.next_state(&values);
        assert_eq!(next, vec![true]); // alarm turns on

        // State: alarm=1, Input: sensor=0 → next_alarm = OR(0,1) = 1
        let mut values = vec![false, false, true]; // [unused, sensor=0, alarm=1]
        circuit.eval_gates(&mut values);
        let next = circuit.next_state(&values);
        assert_eq!(next, vec![true]); // alarm stays on (latched)
    }

    /// The binary form of a 2-bit counter with a justice set — hand-encoded: header, one latch
    /// line per latch (`next [init]`), the justice size + literal, then the ANDs as varint deltas.
    #[test]
    fn binary_aiger_19_decodes_to_the_same_circuit_as_its_ascii_twin() {
        // aag 4 0 2 0 2 0 0 1 0 : l0 (lit 2) next ¬l0 (3); l1 (lit 4) next l0⊕l1 built as
        // a6 = l0 ∧ l1, a8 = ¬a6 ∧ (l0 ∨ l1)… keep it simple: a6 = l0 ∧ l1 (6 2 4), a8 = ¬l0 ∧ ¬l1 (8 3 5);
        // justice {a6}.
        let ascii = "aag 4 0 2 0 2 0 0 1 0\n2 3\n4 7\n1\n6\n6 2 4\n8 3 5\n";
        let from_ascii = parse(ascii).expect("ascii parses");
        let mut bin: Vec<u8> = b"aig 4 0 2 0 2 0 0 1 0\n3\n7\n1\n6\n".to_vec();
        // gate 6 = 2 ∧ 4 : lhs 6, d0 = 6-4 = 2, d1 = 4-2 = 2   (rhs0 is the LARGER operand)
        bin.extend_from_slice(&[2u8, 2u8]);
        // gate 8 = 5 ∧ 3 : lhs 8, d0 = 8-5 = 3, d1 = 5-3 = 2
        bin.extend_from_slice(&[3u8, 2u8]);
        bin.extend_from_slice(b"l0 lo\nl1 hi\nc\nmade by hand\n");
        let from_bin = parse_bytes(&bin).expect("binary parses");
        assert_eq!(from_bin.inputs, from_ascii.inputs);
        assert_eq!(from_bin.latches.len(), 2);
        assert_eq!(
            (
                from_bin.latches[0].current,
                from_bin.latches[0].next,
                from_bin.latches[0].init
            ),
            (2, 3, 0)
        );
        assert_eq!(
            (from_bin.latches[1].current, from_bin.latches[1].next),
            (4, 7)
        );
        let g: Vec<(usize, usize, usize)> = from_bin
            .gates
            .iter()
            .map(|g| (g.lhs, g.rhs0, g.rhs1))
            .collect();
        assert_eq!(g, vec![(6, 4, 2), (8, 5, 3)]);
        assert_eq!(from_bin.justice_sets, vec![vec![6]]);
        assert_eq!(
            from_bin.symbols.latch_names,
            vec![Some("lo".into()), Some("hi".into())]
        );
    }

    #[test]
    fn varints_decode_multi_byte_deltas() {
        let mut p = 0;
        assert_eq!(read_varint(&[0x7f], &mut p), Some(127));
        p = 0;
        assert_eq!(read_varint(&[0x80, 0x01], &mut p), Some(128));
        p = 0;
        assert_eq!(read_varint(&[0xff, 0xff, 0x03], &mut p), Some(65535));
        p = 0;
        assert_eq!(read_varint(&[0x80], &mut p), None, "truncated");
    }
}
