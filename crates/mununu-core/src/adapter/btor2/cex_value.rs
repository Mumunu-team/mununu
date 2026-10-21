//! mununu#723 — a counterexample cell's concrete value at ANY width.
//!
//! The BMC witness, the exact engine's lasso and the report's counterexample all carried a cell
//! as `(name, u64)`; a cell whose value did not fit was silently DROPPED by the witness extractor
//! (`as_u64()` inside a `filter_map`) — on OpenTitan `edn` the violated `EdnFatalAlertNoRsp_A`
//! trace listed every input but `tl_i` (109 bits), the bus write that raises the fatal alert.
//! [`CexValue`] carries the bits whole; the wire shape (`CexCellView`) keeps `value` for every
//! cell that fits (unchanged for every cell that was listed before) and adds `hex` + `bits`.

use std::collections::HashMap;
use std::fmt;

use super::ast::{Btor2File, Node};
use super::parser;

/// A concrete bit-vector value: `bits` wide, LSB-first 64-bit limbs (the top limb masked to the
/// width). Equality is structural, so two cells of the same width compare by value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CexValue {
    bits: u32,
    limbs: Vec<u64>,
}

impl CexValue {
    fn limb_count(bits: u32) -> usize {
        (bits as usize).div_ceil(64).max(1)
    }

    fn masked(mut limbs: Vec<u64>, bits: u32) -> Self {
        let n = Self::limb_count(bits);
        limbs.resize(n, 0);
        let top_bits = bits % 64;
        if top_bits != 0 {
            limbs[n - 1] &= (1u64 << top_bits) - 1;
        } else if bits == 0 {
            limbs[0] = 0;
        }
        CexValue { bits, limbs }
    }

    pub fn from_u64(value: u64, bits: u32) -> Self {
        Self::masked(vec![value], bits)
    }

    pub fn from_u128(value: u128, bits: u32) -> Self {
        Self::masked(vec![value as u64, (value >> 64) as u64], bits)
    }

    /// A binary digit string, MSB first (`x` / `z` read as 0 — Boolector's `as_01x_str`).
    pub fn from_binary_str(s: &str, bits: u32) -> Self {
        let mut limbs = vec![0u64; Self::limb_count(bits)];
        for (i, c) in s.trim().bytes().rev().enumerate() {
            if c == b'1' {
                if i / 64 >= limbs.len() {
                    break;
                }
                limbs[i / 64] |= 1u64 << (i % 64);
            }
        }
        Self::masked(limbs, bits)
    }

    /// A hex digit string, MSB first, no prefix. `None` on a non-hex character.
    pub fn from_hex_str(s: &str, bits: u32) -> Option<Self> {
        let mut limbs = vec![0u64; Self::limb_count(bits)];
        for (i, c) in s.trim().bytes().rev().enumerate() {
            let d = (c as char).to_digit(16)? as u64;
            let bit = i * 4;
            if bit / 64 >= limbs.len() {
                if d != 0 {
                    break;
                }
                continue;
            }
            limbs[bit / 64] |= d << (bit % 64);
        }
        Some(Self::masked(limbs, bits))
    }

    /// An SMT-LIB bit-vector numeral as z3 prints a model value — `#x…`, `#b…`, or a decimal
    /// (`(_ bvN W)` is not produced for values). `None` on anything else.
    pub fn from_smt_numeral(s: &str, bits: u32) -> Option<Self> {
        let s = s.trim();
        if let Some(h) = s.strip_prefix("#x") {
            Self::from_hex_str(h, bits)
        } else if let Some(b) = s.strip_prefix("#b") {
            b.bytes()
                .all(|c| c == b'0' || c == b'1')
                .then(|| Self::from_binary_str(b, bits))
        } else {
            s.parse::<u128>().ok().map(|v| Self::from_u128(v, bits))
        }
    }

    pub fn bits(&self) -> u32 {
        self.bits
    }

    /// The value when it fits 64 bits — every cell that the trace listed before mununu#723.
    pub fn as_u64(&self) -> Option<u64> {
        self.limbs[1..]
            .iter()
            .all(|l| *l == 0)
            .then_some(self.limbs[0])
    }

    /// `0x` + the value in hex, zero-padded to the width (`ceil(bits / 4)` digits).
    pub fn to_hex(&self) -> String {
        let digits = (self.bits as usize).div_ceil(4).max(1);
        let mut out = String::with_capacity(2 + digits);
        out.push_str("0x");
        for i in (0..digits).rev() {
            let bit = i * 4;
            let limb = self.limbs.get(bit / 64).copied().unwrap_or(0);
            let nibble = (limb >> (bit % 64)) & 0xf;
            out.push(char::from_digit(nibble as u32, 16).expect("nibble"));
        }
        out
    }
}

/// Decimal when the value fits 64 bits, the padded hex otherwise — what the CLI prints.
impl fmt::Display for CexValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_u64() {
            Some(v) => write!(f, "{v}"),
            None => f.write_str(&self.to_hex()),
        }
    }
}

/// The width of every named state and input cell of `file`, keyed by the leaf symbol the engines
/// use (`parser::resolve_leaf_symbol` — the same naming as the exact engine's cells and the BMC
/// witness), so a lasso's `u128` values can be given their real width.
pub fn cell_widths(file: &Btor2File) -> HashMap<String, u32> {
    let symbols = parser::collect_symbols(file);
    let mut out = HashMap::new();
    for line in &file.lines {
        let (symbol, kind, sort) = match &line.node {
            Node::State { symbol, sort } => (symbol, "state", *sort),
            Node::Input { symbol, sort } => (symbol, "input", *sort),
            _ => continue,
        };
        let Some(width) = parser::bv_width(file, sort) else {
            continue;
        };
        let name = parser::resolve_leaf_symbol(&symbols, line.nid, symbol, kind);
        out.insert(name, width);
    }
    out
}

/// The `(name, u64)` view of frames whose every value fits 64 bits — for tests that pin a
/// concrete narrow path. Panics on a wide value.
pub fn u64_frames(frames: &[Vec<(String, CexValue)>]) -> Vec<Vec<(String, u64)>> {
    frames
        .iter()
        .map(|f| {
            f.iter()
                .map(|(k, v)| (k.clone(), v.as_u64().expect("a value that fits 64 bits")))
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_a_value_wider_than_64_bits_and_keeps_the_narrow_form() {
        let narrow = CexValue::from_u64(5, 109);
        assert_eq!(narrow.as_u64(), Some(5));
        assert_eq!(narrow.bits(), 109);
        assert_eq!(narrow.to_hex(), format!("0x{:0>28}", "5"));
        let wide = CexValue::from_u128((1u128 << 100) | 5, 109);
        assert_eq!(wide.as_u64(), None, "a value past 64 bits has no u64 form");
        assert_eq!(
            wide.to_hex(),
            format!("0x{:0>28}", "10000000000000000000000005")
        );
        assert_eq!(wide.to_string(), wide.to_hex());
        assert_eq!(narrow.to_string(), "5");
    }

    #[test]
    fn parses_every_numeral_form_the_solvers_print() {
        let hex = CexValue::from_smt_numeral("#x10000000000000005", 68).expect("hex");
        assert_eq!(hex, CexValue::from_u128((1u128 << 64) | 5, 68));
        let bin = CexValue::from_smt_numeral("#b101", 3).expect("bin");
        assert_eq!(bin.as_u64(), Some(5));
        let dec = CexValue::from_smt_numeral("18446744073709551621", 65).expect("dec");
        assert_eq!(dec, CexValue::from_u128((1u128 << 64) | 5, 65));
        assert_eq!(CexValue::from_smt_numeral("(_ bv5 3)", 3), None);
        // Boolector's `01x` string (MSB first), with an `x` read as 0.
        let bx = CexValue::from_binary_str("1x1", 3);
        assert_eq!(bx.as_u64(), Some(5));
        // The top limb is masked to the width.
        assert_eq!(CexValue::from_u64(u64::MAX, 3).as_u64(), Some(7));
    }

    #[test]
    fn cell_widths_name_cells_as_the_engines_do() {
        let file = parser::parse(
            "1 sort bitvec 109\n2 input 1 tl_i\n3 sort bitvec 2\n4 state 3 cnt\n5 state 3\n",
        )
        .unwrap();
        let w = cell_widths(&file);
        assert_eq!(w.get("tl_i"), Some(&109));
        assert_eq!(w.get("cnt"), Some(&2));
        // An anonymous state gets the resolver's placeholder name, as the exact engine's cell does.
        assert!(w.keys().any(|k| k != "tl_i" && k != "cnt"), "{w:?}");
    }
}
