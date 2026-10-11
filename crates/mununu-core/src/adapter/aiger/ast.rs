//! AIGER circuit representation.
//!
//! Represents a synchronous sequential circuit as defined by the AIGER
//! format (version 1.9). The circuit consists of inputs, latches (state
//! elements), AND gates, and output/bad/constraint/justice literals.

/// A complete AIGER circuit.
#[derive(Debug, Clone)]
pub struct Circuit {
    /// Maximum variable index (M in the header).
    pub max_var: usize,
    /// Input literals (even numbers, one per input variable).
    pub inputs: Vec<Literal>,
    /// Latches: (current_literal, next_literal).
    pub latches: Vec<Latch>,
    /// AND gates: (lhs_literal, rhs0_literal, rhs1_literal).
    pub gates: Vec<Gate>,
    /// Plain output literals (`O` in the header), kept for the BTOR2 lowering (named `output`s).
    pub outputs: Vec<Literal>,
    /// Bad-state output literals (safety properties: bad if literal is true).
    pub bad_outputs: Vec<Literal>,
    /// Constraint literals (environment assumptions: must be true).
    pub constraints: Vec<Literal>,
    /// Justice properties (liveness: sets of literals that must be true infinitely often).
    pub justice_sets: Vec<Vec<Literal>>,
    /// Fairness constraints (single literals that must be true infinitely often).
    pub fairness: Vec<Literal>,
    /// Symbol table: maps literals to names.
    pub symbols: SymbolTable,
}

/// A literal in AIGER: even = positive variable, odd = negated variable.
/// Literal 0 = constant false, literal 1 = constant true.
pub type Literal = usize;

/// A latch (state element).
#[derive(Debug, Clone)]
pub struct Latch {
    /// Current state literal (even number = 2 * var_index).
    pub current: Literal,
    /// Next state literal (can be any literal, including negated).
    pub next: Literal,
    /// Initial value: 0 or 1 (default 0), or **2 = undefined** — AIGER 1.9 spells an
    /// uninitialised latch as a third field equal to the latch's own literal.
    pub init: u8,
}

/// An AND gate.
#[derive(Debug, Clone)]
pub struct Gate {
    /// Output literal (even number).
    pub lhs: Literal,
    /// First input literal.
    pub rhs0: Literal,
    /// Second input literal.
    pub rhs1: Literal,
}

/// Symbol table mapping literals to human-readable names.
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    pub input_names: Vec<Option<String>>,
    pub latch_names: Vec<Option<String>>,
    pub output_names: Vec<Option<String>>,
    pub bad_names: Vec<Option<String>>,
    pub constraint_names: Vec<Option<String>>,
}

impl Circuit {
    /// Number of inputs.
    pub fn num_inputs(&self) -> usize {
        self.inputs.len()
    }

    /// Number of latches (state elements).
    pub fn num_latches(&self) -> usize {
        self.latches.len()
    }

    /// Evaluate a literal given a variable assignment.
    /// `values[i]` = value of variable i (0 or 1).
    pub fn eval_literal(&self, lit: Literal, values: &[bool]) -> bool {
        if lit == 0 {
            return false; // constant false
        }
        if lit == 1 {
            return true; // constant true
        }
        let var_idx = lit / 2;
        let negated = lit % 2 == 1;
        let val = values.get(var_idx).copied().unwrap_or(false);
        if negated { !val } else { val }
    }

    /// Evaluate all AND gates and return the full variable assignment.
    /// Input: values[0] = unused (constant), values[1..I] = inputs, values[I+1..I+L] = latches.
    /// Output: values extended with gate outputs.
    pub fn eval_gates(&self, values: &mut Vec<bool>) {
        // Ensure enough space for all variables
        values.resize(self.max_var + 1, false);

        // Evaluate each AND gate in order (they are topologically sorted in AIGER)
        for gate in &self.gates {
            let rhs0_val = self.eval_literal(gate.rhs0, values);
            let rhs1_val = self.eval_literal(gate.rhs1, values);
            let result = rhs0_val && rhs1_val;
            let var_idx = gate.lhs / 2;
            if var_idx < values.len() {
                values[var_idx] = result;
            }
        }
    }

    /// Compute the next-state for each latch given a full variable assignment.
    pub fn next_state(&self, values: &[bool]) -> Vec<bool> {
        self.latches
            .iter()
            .map(|latch| self.eval_literal(latch.next, values))
            .collect()
    }

    /// Get the name for an input by index.
    pub fn input_name(&self, idx: usize) -> String {
        self.symbols
            .input_names
            .get(idx)
            .and_then(|n| n.as_ref())
            .cloned()
            .unwrap_or_else(|| format!("i{idx}"))
    }

    /// Get the name for a latch by index.
    pub fn latch_name(&self, idx: usize) -> String {
        self.symbols
            .latch_names
            .get(idx)
            .and_then(|n| n.as_ref())
            .cloned()
            .unwrap_or_else(|| format!("l{idx}"))
    }
}
