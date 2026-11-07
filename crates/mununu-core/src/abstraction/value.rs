//! Abstract value representation for state unrolling.
//!
//! This module implements the complete abstract value type system for state unrolling. (The
//! design note that specified it was never tracked in this repository; the types below are the
//! specification.)
//!
//! The type system supports:
//! - Concrete values: `IntConstant`, `BoolConstant`, `SymbolConstant`
//! - Abstract integer values: `IntInterval`, `IntSet`, `IntTop`
//! - Abstract boolean values: `BoolSet`
//! - Abstract symbol values: `SymbolSet`, `SymbolTop`
//! - Special values: `PositiveInfinity`, `NegativeInfinity`, `Undefined`

use std::collections::HashSet;
use std::fmt;

/// Abstract value representing a variable's value during state unrolling.
///
/// This enum represents all possible abstract values that can be assigned to variables.
/// Values are normalized to ensure canonical representation (see normalization rules).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbstractValue {
    // === Concrete Values ===
    /// Concrete integer constant.
    IntConstant(i64),
    /// Concrete boolean constant.
    BoolConstant(bool),
    /// Concrete symbol constant (string).
    SymbolConstant(String),

    // === Abstract Integer Values ===
    /// Integer interval `[min, max]` (inclusive bounds).
    /// Represents all integers between `min` and `max` (inclusive).
    /// If `min == max`, this should be normalized to `IntConstant(min)`.
    IntInterval(i64, i64),
    /// Integer set (discrete set of integers).
    /// Represents that the value is one of the integers in the set.
    /// If the set has a single element, this should be normalized to `IntConstant(value)`.
    IntSet(HashSet<i64>),
    /// Integer top (all integers, unbounded).
    /// Should be avoided in unrolling as it represents an unbounded domain.
    IntTop,

    // === Abstract Boolean Values ===
    /// Boolean set (discrete set of booleans).
    /// Can be `{true}`, `{false}`, or `{true, false}`.
    /// If the set has a single element, this should be normalized to `BoolConstant(value)`.
    /// `{true, false}` represents unknown boolean (top element).
    BoolSet(HashSet<bool>),

    // === Abstract Symbol Values ===
    /// Symbol set (discrete set of symbols/strings).
    /// Represents that the value is one of the symbols in the set.
    /// If the set has a single element, this should be normalized to `SymbolConstant(value)`.
    SymbolSet(HashSet<String>),
    /// Symbol top (all symbols, unbounded).
    /// Should be avoided in unrolling as it represents an unbounded domain.
    SymbolTop,

    // === Special Values ===
    /// Positive infinity (result of overflow or unbounded upper bound).
    PositiveInfinity,
    /// Negative infinity (result of underflow or unbounded lower bound).
    NegativeInfinity,
    /// Undefined value (result of division by zero or invalid operations).
    Undefined,
}

impl AbstractValue {
    // === Constructor Methods ===

    /// Creates a concrete integer constant.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let val = AbstractValue::int_constant(42);
    /// ```
    pub fn int_constant(value: i64) -> Self {
        Self::IntConstant(value)
    }

    /// Creates a concrete boolean constant.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let val = AbstractValue::bool_constant(true);
    /// ```
    pub fn bool_constant(value: bool) -> Self {
        Self::BoolConstant(value)
    }

    /// Creates a concrete symbol constant.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let val = AbstractValue::symbol_constant("pending".to_string());
    /// ```
    pub fn symbol_constant(value: String) -> Self {
        Self::SymbolConstant(value)
    }

    /// Creates an integer interval `[min, max]` (inclusive).
    ///
    /// # Panics
    /// Panics if `min > max`.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let val = AbstractValue::int_interval(0, 10);
    /// ```
    pub fn int_interval(min: i64, max: i64) -> Self {
        assert!(
            min <= max,
            "interval min ({}) must be <= max ({})",
            min,
            max
        );
        Self::IntInterval(min, max)
    }

    /// Creates an integer set from a vector of integers.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let val = AbstractValue::int_set(vec![0, 1, 2]);
    /// ```
    pub fn int_set(values: Vec<i64>) -> Self {
        Self::IntSet(values.into_iter().collect())
    }

    /// Creates an integer top (unbounded).
    ///
    /// # Warning
    /// `IntTop` represents an unbounded domain and should be avoided in unrolling.
    /// Use this only when bounds cannot be determined.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let val = AbstractValue::int_top();
    /// ```
    pub fn int_top() -> Self {
        Self::IntTop
    }

    /// Creates a boolean set from a vector of booleans.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let val = AbstractValue::bool_set(vec![true, false]);
    /// ```
    pub fn bool_set(values: Vec<bool>) -> Self {
        Self::BoolSet(values.into_iter().collect())
    }

    /// Creates a symbol set from a vector of strings.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let val = AbstractValue::symbol_set(vec!["pending".to_string(), "active".to_string()]);
    /// ```
    pub fn symbol_set(values: Vec<String>) -> Self {
        Self::SymbolSet(values.into_iter().collect())
    }

    /// Creates a symbol top (unbounded).
    ///
    /// # Warning
    /// `SymbolTop` represents an unbounded domain and should be avoided in unrolling.
    /// Use this only when the symbol set cannot be enumerated.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let val = AbstractValue::symbol_top();
    /// ```
    pub fn symbol_top() -> Self {
        Self::SymbolTop
    }

    // === Accessor Methods ===

    /// Returns `true` if this is a concrete integer constant.
    pub fn is_int_constant(&self) -> bool {
        matches!(self, Self::IntConstant(_))
    }

    /// Returns the integer value if this is a concrete integer constant.
    pub fn as_int_constant(&self) -> Option<i64> {
        match self {
            Self::IntConstant(n) => Some(*n),
            _ => None,
        }
    }

    /// Returns `true` if this is a concrete boolean constant.
    pub fn is_bool_constant(&self) -> bool {
        matches!(self, Self::BoolConstant(_))
    }

    /// Returns the boolean value if this is a concrete boolean constant.
    pub fn as_bool_constant(&self) -> Option<bool> {
        match self {
            Self::BoolConstant(b) => Some(*b),
            _ => None,
        }
    }

    /// Returns `true` if this is a concrete symbol constant.
    pub fn is_symbol_constant(&self) -> bool {
        matches!(self, Self::SymbolConstant(_))
    }

    /// Returns the symbol value if this is a concrete symbol constant.
    pub fn as_symbol_constant(&self) -> Option<&String> {
        match self {
            Self::SymbolConstant(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the interval bounds if this is an integer interval.
    pub fn as_int_interval(&self) -> Option<(i64, i64)> {
        match self {
            Self::IntInterval(min, max) => Some((*min, *max)),
            _ => None,
        }
    }

    /// Returns the integer set if this is an integer set.
    pub fn as_int_set(&self) -> Option<&HashSet<i64>> {
        match self {
            Self::IntSet(set) => Some(set),
            _ => None,
        }
    }

    /// Returns the boolean set if this is a boolean set.
    pub fn as_bool_set(&self) -> Option<&HashSet<bool>> {
        match self {
            Self::BoolSet(set) => Some(set),
            _ => None,
        }
    }

    /// Returns the symbol set if this is a symbol set.
    pub fn as_symbol_set(&self) -> Option<&HashSet<String>> {
        match self {
            Self::SymbolSet(set) => Some(set),
            _ => None,
        }
    }

    /// Returns `true` if this is `IntTop`.
    pub fn is_int_top(&self) -> bool {
        matches!(self, Self::IntTop)
    }

    /// Returns `true` if this is `SymbolTop`.
    pub fn is_symbol_top(&self) -> bool {
        matches!(self, Self::SymbolTop)
    }

    /// Returns `true` if this is `PositiveInfinity`.
    pub fn is_positive_infinity(&self) -> bool {
        matches!(self, Self::PositiveInfinity)
    }

    /// Returns `true` if this is `NegativeInfinity`.
    pub fn is_negative_infinity(&self) -> bool {
        matches!(self, Self::NegativeInfinity)
    }

    /// Returns `true` if this is `Undefined`.
    pub fn is_undefined(&self) -> bool {
        matches!(self, Self::Undefined)
    }

    // === Type Checking Methods ===

    /// Returns `true` if this is an integer value (any integer variant).
    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Self::IntConstant(_)
                | Self::IntInterval(_, _)
                | Self::IntSet(_)
                | Self::IntTop
                | Self::PositiveInfinity
                | Self::NegativeInfinity
        )
    }

    /// Returns `true` if this is a boolean value (any boolean variant).
    pub fn is_boolean(&self) -> bool {
        matches!(self, Self::BoolConstant(_) | Self::BoolSet(_))
    }

    /// Returns `true` if this is a symbol value (any symbol variant).
    pub fn is_symbol(&self) -> bool {
        matches!(
            self,
            Self::SymbolConstant(_) | Self::SymbolSet(_) | Self::SymbolTop
        )
    }

    // === Compatibility Methods (for backward compatibility) ===

    /// Creates a boolean abstract value (backward compatibility).
    ///
    /// This method is provided for backward compatibility with existing code.
    /// Prefer `bool_constant()` for new code.
    #[deprecated(note = "Use bool_constant() instead")]
    pub fn bool(value: bool) -> Self {
        Self::bool_constant(value)
    }

    /// Returns the boolean domain if this is a boolean value (backward compatibility).
    ///
    /// This method converts the new `AbstractValue` structure to the old `BoolDomain` enum.
    /// Used for backward compatibility with existing code.
    #[deprecated(note = "Use as_bool_constant() or as_bool_set() instead")]
    pub fn as_bool(&self) -> Option<crate::abstraction::domains::BoolDomain> {
        use crate::abstraction::domains::BoolDomain;
        match self {
            Self::BoolConstant(true) => Some(BoolDomain::True),
            Self::BoolConstant(false) => Some(BoolDomain::False),
            Self::BoolSet(set) => {
                if set.contains(&true) && set.contains(&false) {
                    Some(BoolDomain::Unknown)
                } else if set.contains(&true) {
                    Some(BoolDomain::True)
                } else if set.contains(&false) {
                    Some(BoolDomain::False)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Returns the integer domain if this is an integer value (backward compatibility).
    ///
    /// This method converts the new `AbstractValue` structure to the old `IntDomain` struct.
    /// Used for backward compatibility with existing code.
    #[deprecated(note = "Use as_int_constant(), as_int_interval(), or as_int_set() instead")]
    pub fn as_int(&self) -> Option<crate::abstraction::domains::IntDomain> {
        use crate::abstraction::domains::IntDomain;
        match self {
            Self::IntConstant(n) => Some(IntDomain::constant(*n)),
            Self::IntInterval(min, max) => Some(IntDomain::interval(Some(*min), Some(*max))),
            Self::IntSet(set) => {
                if set.is_empty() {
                    None
                } else {
                    let min = *set.iter().min().unwrap();
                    let max = *set.iter().max().unwrap();
                    Some(IntDomain::interval(Some(min), Some(max)))
                }
            }
            Self::IntTop => Some(IntDomain::unbounded()),
            Self::PositiveInfinity => Some(IntDomain::interval(Some(i64::MAX), None)),
            Self::NegativeInfinity => Some(IntDomain::interval(None, Some(i64::MIN))),
            _ => None,
        }
    }

    /// Checks if this value is unknown (backward compatibility).
    ///
    /// This method is provided for backward compatibility.
    /// In the new type system, "unknown" is represented by top values or sets containing all values.
    #[deprecated(note = "Use is_int_top() or is_symbol_top() instead")]
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::IntTop | Self::SymbolTop)
    }

    // === Comparison Operations ===

    /// Compares two abstract values for equality.
    ///
    /// Returns `Some(true)` if definitely equal, `Some(false)` if definitely not equal,
    /// or `None` if the result is unknown (maybe equal).
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let val1 = AbstractValue::int_constant(5);
    /// let val2 = AbstractValue::int_constant(5);
    /// assert_eq!(val1.compare_eq(&val2), Some(true));
    ///
    /// let val3 = AbstractValue::int_interval(0, 10);
    /// let val4 = AbstractValue::int_constant(5);
    /// assert_eq!(val3.compare_eq(&val4), None); // Maybe equal
    /// ```
    pub fn compare_eq(&self, other: &Self) -> Option<bool> {
        match (self, other) {
            // Concrete values: exact comparison
            (Self::IntConstant(a), Self::IntConstant(b)) => Some(a == b),
            (Self::BoolConstant(a), Self::BoolConstant(b)) => Some(a == b),
            (Self::SymbolConstant(a), Self::SymbolConstant(b)) => Some(a == b),

            // Interval comparisons
            (Self::IntInterval(min1, max1), Self::IntInterval(min2, max2)) => {
                if max1 < min2 || max2 < min1 {
                    Some(false) // No overlap
                } else if min1 == max1 && min2 == max2 && min1 == min2 {
                    Some(true) // Both are constants and equal
                } else {
                    None // May be equal
                }
            }
            (Self::IntConstant(n), Self::IntInterval(min, max)) => {
                if *n < *min || *n > *max {
                    Some(false)
                } else {
                    None // May be equal if interval contains the constant
                }
            }
            (Self::IntInterval(_min, _max), Self::IntConstant(n)) => {
                // Symmetric case: check if constant is in interval
                if *n < *_min || *n > *_max {
                    Some(false)
                } else {
                    None
                }
            }

            // Set comparisons
            (Self::IntSet(set1), Self::IntSet(set2)) => {
                if set1 == set2 {
                    Some(true)
                } else if set1.is_disjoint(set2) {
                    Some(false)
                } else {
                    None // May be equal if sets overlap
                }
            }
            (Self::IntConstant(n), Self::IntSet(set)) => {
                if set.contains(n) {
                    if set.len() == 1 {
                        Some(true)
                    } else {
                        None // May be equal
                    }
                } else {
                    Some(false)
                }
            }
            (Self::IntSet(_set), Self::IntConstant(n)) => {
                // Symmetric case
                if _set.contains(n) {
                    if _set.len() == 1 { Some(true) } else { None }
                } else {
                    Some(false)
                }
            }

            // Boolean set comparisons
            (Self::BoolSet(set1), Self::BoolSet(set2)) => {
                if set1 == set2 {
                    Some(true)
                } else if set1.is_disjoint(set2) {
                    Some(false)
                } else {
                    None
                }
            }
            (Self::BoolConstant(b), Self::BoolSet(set)) => {
                if set.contains(b) {
                    if set.len() == 1 { Some(true) } else { None }
                } else {
                    Some(false)
                }
            }
            (Self::BoolSet(_set), Self::BoolConstant(b)) => {
                // Symmetric case
                if _set.contains(b) {
                    if _set.len() == 1 { Some(true) } else { None }
                } else {
                    Some(false)
                }
            }

            // Symbol set comparisons
            (Self::SymbolSet(set1), Self::SymbolSet(set2)) => {
                if set1 == set2 {
                    Some(true)
                } else if set1.is_disjoint(set2) {
                    Some(false)
                } else {
                    None
                }
            }
            (Self::SymbolConstant(s), Self::SymbolSet(set)) => {
                if set.contains(s) {
                    if set.len() == 1 { Some(true) } else { None }
                } else {
                    Some(false)
                }
            }
            (Self::SymbolSet(_set), Self::SymbolConstant(s)) => {
                // Symmetric case
                if _set.contains(s) {
                    if _set.len() == 1 { Some(true) } else { None }
                } else {
                    Some(false)
                }
            }

            // Top values: always unknown
            (Self::IntTop, _) | (_, Self::IntTop) => None,
            (Self::SymbolTop, _) | (_, Self::SymbolTop) => None,

            // Special values
            (Self::PositiveInfinity, Self::PositiveInfinity) => Some(true),
            (Self::NegativeInfinity, Self::NegativeInfinity) => Some(true),
            (Self::Undefined, Self::Undefined) => Some(true),
            (Self::PositiveInfinity, _) | (_, Self::PositiveInfinity) => Some(false),
            (Self::NegativeInfinity, _) | (_, Self::NegativeInfinity) => Some(false),
            (Self::Undefined, _) | (_, Self::Undefined) => Some(false),

            // Different types: not equal
            _ => Some(false),
        }
    }

    /// Compares two abstract values for less-than ordering.
    ///
    /// Returns `Some(true)` if definitely less, `Some(false)` if definitely not less,
    /// or `None` if the result is unknown.
    ///
    /// Only works for integer values.
    pub fn compare_lt(&self, other: &Self) -> Option<bool> {
        match (self, other) {
            // Concrete values: exact comparison
            (Self::IntConstant(a), Self::IntConstant(b)) => Some(a < b),

            // Interval comparisons
            (Self::IntInterval(min1, max1), Self::IntInterval(min2, max2)) => {
                if max1 < min2 {
                    Some(true) // Definitely less
                } else if min1 >= max2 {
                    Some(false) // Definitely not less
                } else {
                    None // May be less
                }
            }
            (Self::IntConstant(n), Self::IntInterval(min, _)) => {
                if *n < *min {
                    Some(true)
                } else {
                    None // May be less if n is in interval
                }
            }
            (Self::IntInterval(_, max), Self::IntConstant(n)) => {
                if max < n {
                    Some(true)
                } else {
                    None
                }
            }

            // Set comparisons
            (Self::IntSet(set1), Self::IntSet(set2)) => {
                let max1 = set1.iter().max()?;
                let min2 = set2.iter().min()?;
                if max1 < min2 {
                    Some(true)
                } else {
                    let min1 = set1.iter().min()?;
                    let max2 = set2.iter().max()?;
                    if min1 >= max2 {
                        Some(false)
                    } else {
                        None // May be less
                    }
                }
            }
            (Self::IntConstant(n), Self::IntSet(set)) => {
                let max = set.iter().max()?;
                if *n < *max {
                    if *n < *set.iter().min().unwrap() {
                        Some(true)
                    } else {
                        None
                    }
                } else {
                    Some(false)
                }
            }
            (Self::IntSet(set), Self::IntConstant(n)) => {
                let min = set.iter().min()?;
                if *min < *n {
                    if *set.iter().max().unwrap() < *n {
                        Some(true)
                    } else {
                        None
                    }
                } else {
                    Some(false)
                }
            }

            // Special values
            (Self::NegativeInfinity, _) => Some(true),
            (_, Self::PositiveInfinity) => Some(true),
            (Self::PositiveInfinity, _) => Some(false),
            (_, Self::NegativeInfinity) => Some(false),
            (Self::Undefined, _) | (_, Self::Undefined) => None,

            // Top values: unknown
            (Self::IntTop, _) | (_, Self::IntTop) => None,

            // Different types or non-integer: not comparable
            _ => None,
        }
    }

    /// Compares two abstract values for less-than-or-equal ordering.
    pub fn compare_le(&self, other: &Self) -> Option<bool> {
        match self.compare_lt(other) {
            Some(true) => Some(true),
            Some(false) => self.compare_eq(other), // If not less, check if equal
            None => None,
        }
    }

    /// Compares two abstract values for greater-than ordering.
    pub fn compare_gt(&self, other: &Self) -> Option<bool> {
        other.compare_lt(self) // Symmetric
    }

    /// Compares two abstract values for greater-than-or-equal ordering.
    pub fn compare_ge(&self, other: &Self) -> Option<bool> {
        other.compare_le(self) // Symmetric
    }

    // === Normalization ===

    /// Normalizes an abstract value to ensure canonical representation.
    ///
    /// Normalization rules:
    /// - `IntInterval(n, n)` → `IntConstant(n)`
    /// - `IntSet({n})` → `IntConstant(n)`
    /// - `BoolSet({b})` → `BoolConstant(b)`
    /// - `SymbolSet({s})` → `SymbolConstant(s)`
    ///
    /// This function is idempotent: `normalize(normalize(x)) == normalize(x)`.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let interval = AbstractValue::int_interval(5, 5);
    /// let normalized = interval.normalize();
    /// assert_eq!(normalized, AbstractValue::int_constant(5));
    ///
    /// let set = AbstractValue::int_set(vec![42]);
    /// let normalized = set.normalize();
    /// assert_eq!(normalized, AbstractValue::int_constant(42));
    /// ```
    pub fn normalize(self) -> Self {
        match self {
            // Concrete values are already normalized
            Self::IntConstant(_) | Self::BoolConstant(_) | Self::SymbolConstant(_) => self,

            // Normalize intervals: [n, n] → IntConstant(n)
            Self::IntInterval(min, max) => {
                if min == max {
                    Self::IntConstant(min)
                } else {
                    Self::IntInterval(min, max)
                }
            }

            // Normalize integer sets: {n} → IntConstant(n)
            Self::IntSet(set) => {
                if set.is_empty() {
                    // Empty set is invalid, but we'll keep it as-is (could be an error)
                    Self::IntSet(set)
                } else if set.len() == 1 {
                    Self::IntConstant(*set.iter().next().unwrap())
                } else {
                    Self::IntSet(set)
                }
            }

            // IntTop is already normalized
            Self::IntTop => self,

            // Normalize boolean sets: {b} → BoolConstant(b)
            Self::BoolSet(set) => {
                if set.is_empty() {
                    // Empty set is invalid, but we'll keep it as-is (could be an error)
                    Self::BoolSet(set)
                } else if set.len() == 1 {
                    Self::BoolConstant(*set.iter().next().unwrap())
                } else {
                    Self::BoolSet(set)
                }
            }

            // Normalize symbol sets: {s} → SymbolConstant(s)
            Self::SymbolSet(set) => {
                if set.is_empty() {
                    // Empty set is invalid, but we'll keep it as-is (could be an error)
                    Self::SymbolSet(set)
                } else if set.len() == 1 {
                    Self::SymbolConstant(set.into_iter().next().unwrap())
                } else {
                    Self::SymbolSet(set)
                }
            }

            // SymbolTop is already normalized
            Self::SymbolTop => self,

            // Special values are already normalized
            Self::PositiveInfinity | Self::NegativeInfinity | Self::Undefined => self,
        }
    }

    /// Normalizes an abstract value in place (consumes and returns normalized value).
    ///
    /// This is a convenience method that calls `normalize()`.
    /// Use this when you want to normalize a value that you're about to use.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let mut val = AbstractValue::int_interval(5, 5);
    /// val = val.normalize();
    /// assert_eq!(val, AbstractValue::int_constant(5));
    /// ```
    #[inline]
    pub fn normalized(self) -> Self {
        self.normalize()
    }

    // === Arithmetic Operations ===

    /// Adds two abstract values.
    ///
    /// Handles all type combinations and overflow/underflow.
    /// All results are normalized.
    ///
    /// # Examples
    ///
    /// ## Concrete values
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let a = AbstractValue::int_constant(5);
    /// let b = AbstractValue::int_constant(10);
    /// assert_eq!(a.add(&b), AbstractValue::int_constant(15));
    /// ```
    ///
    /// ## Interval arithmetic
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let a = AbstractValue::int_interval(0, 5);
    /// let b = AbstractValue::int_interval(10, 20);
    /// assert_eq!(a.add(&b), AbstractValue::int_interval(10, 25));
    /// ```
    ///
    /// ## Set arithmetic
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let a = AbstractValue::int_set(vec![1, 2, 3]);
    /// let b = AbstractValue::int_constant(10);
    /// let result = a.add(&b);
    /// // Result is {11, 12, 13}
    /// ```
    ///
    /// ## Overflow handling
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let a = AbstractValue::int_constant(i64::MAX);
    /// let b = AbstractValue::int_constant(1);
    /// assert_eq!(a.add(&b), AbstractValue::PositiveInfinity);
    /// ```
    pub fn add(&self, other: &Self) -> Self {
        match (self, other) {
            // Concrete + Concrete
            (Self::IntConstant(a), Self::IntConstant(b)) => {
                match a.checked_add(*b) {
                    Some(n) => Self::IntConstant(n),
                    None => {
                        // Overflow: determine sign
                        if *a > 0 && *b >= 0 {
                            Self::PositiveInfinity
                        } else {
                            Self::NegativeInfinity
                        }
                    }
                }
            }

            // Interval + Interval
            (Self::IntInterval(a_min, a_max), Self::IntInterval(b_min, b_max)) => {
                let min_result = a_min.checked_add(*b_min);
                let max_result = a_max.checked_add(*b_max);

                match (min_result, max_result) {
                    (Some(min), Some(max)) => {
                        if min == max {
                            Self::IntConstant(min)
                        } else {
                            Self::IntInterval(min, max)
                        }
                    }
                    (Some(min), None) => {
                        // Upper bound overflowed
                        Self::IntInterval(min, i64::MAX).normalize()
                    }
                    (None, Some(max)) => {
                        // Lower bound underflowed
                        Self::IntInterval(i64::MIN, max).normalize()
                    }
                    (None, None) => {
                        // Both bounds overflowed/underflowed
                        Self::IntTop
                    }
                }
            }

            // Constant + Interval (symmetric)
            (Self::IntConstant(a), Self::IntInterval(b_min, b_max)) => {
                // Implement directly to avoid recursion
                let min_result = a.checked_add(*b_min);
                let max_result = a.checked_add(*b_max);

                match (min_result, max_result) {
                    (Some(min), Some(max)) => {
                        if min == max {
                            Self::IntConstant(min)
                        } else {
                            Self::IntInterval(min, max)
                        }
                    }
                    (Some(min), None) => {
                        // Upper bound overflowed
                        Self::IntInterval(min, i64::MAX).normalize()
                    }
                    (None, Some(max)) => {
                        // Lower bound underflowed
                        Self::IntInterval(i64::MIN, max).normalize()
                    }
                    (None, None) => {
                        // Both bounds overflowed/underflowed
                        Self::IntTop
                    }
                }
            }
            (Self::IntInterval(a_min, a_max), Self::IntConstant(b)) => {
                // Symmetric case: implement directly to avoid recursion
                let min_result = a_min.checked_add(*b);
                let max_result = a_max.checked_add(*b);

                match (min_result, max_result) {
                    (Some(min), Some(max)) => {
                        if min == max {
                            Self::IntConstant(min)
                        } else {
                            Self::IntInterval(min, max)
                        }
                    }
                    (Some(min), None) => {
                        // Upper bound overflowed
                        Self::IntInterval(min, i64::MAX).normalize()
                    }
                    (None, Some(max)) => {
                        // Lower bound underflowed
                        Self::IntInterval(i64::MIN, max).normalize()
                    }
                    (None, None) => {
                        // Both bounds overflowed/underflowed
                        Self::IntTop
                    }
                }
            }

            // Set + Set
            (Self::IntSet(a_set), Self::IntSet(b_set)) => {
                let mut result_set = HashSet::new();
                for a in a_set {
                    for b in b_set {
                        if let Some(sum) = a.checked_add(*b) {
                            result_set.insert(sum);
                        } else {
                            // Overflow in set addition - result is unbounded
                            return Self::IntTop;
                        }
                    }
                }
                if result_set.is_empty() {
                    Self::IntTop
                } else {
                    Self::IntSet(result_set).normalize()
                }
            }

            // Constant + Set (symmetric)
            (Self::IntConstant(a), Self::IntSet(b_set)) => {
                let mut result_set = HashSet::new();
                for b in b_set {
                    if let Some(sum) = a.checked_add(*b) {
                        result_set.insert(sum);
                    } else {
                        return Self::IntTop;
                    }
                }
                Self::IntSet(result_set).normalize()
            }
            (Self::IntSet(a_set), Self::IntConstant(b)) => {
                // Symmetric case: implement directly to avoid recursion
                let mut result_set = HashSet::new();
                for a in a_set {
                    if let Some(sum) = a.checked_add(*b) {
                        result_set.insert(sum);
                    } else {
                        return Self::IntTop;
                    }
                }
                Self::IntSet(result_set).normalize()
            }

            // Interval + Set (symmetric)
            (Self::IntInterval(a_min, a_max), Self::IntSet(b_set)) => {
                // Convert set to interval bounds and add
                if let (Some(b_min), Some(b_max)) =
                    (b_set.iter().min().copied(), b_set.iter().max().copied())
                {
                    Self::IntInterval(*a_min, *a_max).add(&Self::IntInterval(b_min, b_max))
                } else {
                    Self::IntTop
                }
            }
            (Self::IntSet(a_set), Self::IntInterval(b_min, b_max)) => {
                // Symmetric case: implement directly to avoid recursion
                if let (Some(a_min), Some(a_max)) =
                    (a_set.iter().min().copied(), a_set.iter().max().copied())
                {
                    Self::IntInterval(a_min, a_max).add(&Self::IntInterval(*b_min, *b_max))
                } else {
                    Self::IntTop
                }
            }

            // Top values
            (Self::IntTop, _) | (_, Self::IntTop) => Self::IntTop,

            // Special values
            (Self::PositiveInfinity, _) | (_, Self::PositiveInfinity) => {
                if matches!(other, Self::NegativeInfinity) || matches!(self, Self::NegativeInfinity)
                {
                    Self::IntTop // +∞ + (-∞) = unknown
                } else {
                    Self::PositiveInfinity
                }
            }
            (Self::NegativeInfinity, _) | (_, Self::NegativeInfinity) => {
                if matches!(other, Self::PositiveInfinity) || matches!(self, Self::PositiveInfinity)
                {
                    Self::IntTop // (-∞) + (+∞) = unknown
                } else {
                    Self::NegativeInfinity
                }
            }
            (Self::Undefined, _) | (_, Self::Undefined) => Self::Undefined,

            // Type mismatch - return undefined
            _ => Self::Undefined,
        }
    }

    /// Subtracts two abstract values.
    ///
    /// Handles all type combinations and overflow/underflow.
    /// All results are normalized.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let a = AbstractValue::int_constant(10);
    /// let b = AbstractValue::int_constant(5);
    /// assert_eq!(a.sub(&b), AbstractValue::int_constant(5));
    ///
    /// let a = AbstractValue::int_interval(10, 20);
    /// let b = AbstractValue::int_interval(0, 5);
    /// assert_eq!(a.sub(&b), AbstractValue::int_interval(5, 20));
    /// ```
    pub fn sub(&self, other: &Self) -> Self {
        match (self, other) {
            // Concrete - Concrete
            (Self::IntConstant(a), Self::IntConstant(b)) => {
                match a.checked_sub(*b) {
                    Some(n) => Self::IntConstant(n),
                    None => {
                        // Underflow: a < b and both are large
                        if *a < *b {
                            Self::NegativeInfinity
                        } else {
                            Self::PositiveInfinity
                        }
                    }
                }
            }

            // Interval - Interval: [a_min, a_max] - [b_min, b_max] = [a_min - b_max, a_max - b_min]
            (Self::IntInterval(a_min, a_max), Self::IntInterval(b_min, b_max)) => {
                let min_result = a_min.checked_sub(*b_max);
                let max_result = a_max.checked_sub(*b_min);

                match (min_result, max_result) {
                    (Some(min), Some(max)) => {
                        if min == max {
                            Self::IntConstant(min)
                        } else {
                            Self::IntInterval(min, max)
                        }
                    }
                    (Some(min), None) => {
                        // Upper bound overflowed
                        Self::IntInterval(min, i64::MAX).normalize()
                    }
                    (None, Some(max)) => {
                        // Lower bound underflowed
                        Self::IntInterval(i64::MIN, max).normalize()
                    }
                    (None, None) => {
                        // Both bounds overflowed/underflowed
                        Self::IntTop
                    }
                }
            }

            // Constant - Interval
            (Self::IntConstant(a), Self::IntInterval(b_min, b_max)) => {
                // Implement directly to avoid recursion
                // a - [b_min, b_max] = [a - b_max, a - b_min]
                let min_result = a.checked_sub(*b_max);
                let max_result = a.checked_sub(*b_min);

                match (min_result, max_result) {
                    (Some(min), Some(max)) => {
                        if min == max {
                            Self::IntConstant(min)
                        } else {
                            Self::IntInterval(min, max)
                        }
                    }
                    (Some(min), None) => {
                        // Upper bound overflowed
                        Self::IntInterval(min, i64::MAX).normalize()
                    }
                    (None, Some(max)) => {
                        // Lower bound underflowed
                        Self::IntInterval(i64::MIN, max).normalize()
                    }
                    (None, None) => {
                        // Both bounds overflowed/underflowed
                        Self::IntTop
                    }
                }
            }
            (Self::IntInterval(a_min, a_max), Self::IntConstant(b)) => {
                // Symmetric case: implement directly to avoid recursion
                // [a_min, a_max] - b = [a_min - b, a_max - b]
                let min_result = a_min.checked_sub(*b);
                let max_result = a_max.checked_sub(*b);

                match (min_result, max_result) {
                    (Some(min), Some(max)) => {
                        if min == max {
                            Self::IntConstant(min)
                        } else {
                            Self::IntInterval(min, max)
                        }
                    }
                    (Some(min), None) => {
                        // Upper bound overflowed
                        Self::IntInterval(min, i64::MAX).normalize()
                    }
                    (None, Some(max)) => {
                        // Lower bound underflowed
                        Self::IntInterval(i64::MIN, max).normalize()
                    }
                    (None, None) => {
                        // Both bounds overflowed/underflowed
                        Self::IntTop
                    }
                }
            }

            // Set - Set
            (Self::IntSet(a_set), Self::IntSet(b_set)) => {
                let mut result_set = HashSet::new();
                for a in a_set {
                    for b in b_set {
                        if let Some(diff) = a.checked_sub(*b) {
                            result_set.insert(diff);
                        } else {
                            // Underflow in set subtraction - result is unbounded
                            return Self::IntTop;
                        }
                    }
                }
                if result_set.is_empty() {
                    Self::IntTop
                } else {
                    Self::IntSet(result_set).normalize()
                }
            }

            // Constant - Set (symmetric)
            (Self::IntConstant(a), Self::IntSet(b_set)) => {
                let mut result_set = HashSet::new();
                for b in b_set {
                    if let Some(diff) = a.checked_sub(*b) {
                        result_set.insert(diff);
                    } else {
                        return Self::IntTop;
                    }
                }
                Self::IntSet(result_set).normalize()
            }
            (Self::IntSet(a_set), Self::IntConstant(b)) => {
                let mut result_set = HashSet::new();
                for a in a_set {
                    if let Some(diff) = a.checked_sub(*b) {
                        result_set.insert(diff);
                    } else {
                        return Self::IntTop;
                    }
                }
                Self::IntSet(result_set).normalize()
            }

            // Interval - Set (symmetric)
            (Self::IntInterval(a_min, a_max), Self::IntSet(b_set)) => {
                if let (Some(b_min), Some(b_max)) =
                    (b_set.iter().min().copied(), b_set.iter().max().copied())
                {
                    Self::IntInterval(*a_min, *a_max).sub(&Self::IntInterval(b_min, b_max))
                } else {
                    Self::IntTop
                }
            }
            (Self::IntSet(a_set), Self::IntInterval(b_min, b_max)) => {
                if let (Some(a_min), Some(a_max)) =
                    (a_set.iter().min().copied(), a_set.iter().max().copied())
                {
                    Self::IntInterval(a_min, a_max).sub(&Self::IntInterval(*b_min, *b_max))
                } else {
                    Self::IntTop
                }
            }

            // Top values
            (Self::IntTop, _) | (_, Self::IntTop) => Self::IntTop,

            // Special values
            (Self::PositiveInfinity, Self::PositiveInfinity) => Self::IntTop, // +∞ - (+∞) = unknown
            (Self::NegativeInfinity, Self::NegativeInfinity) => Self::IntTop, // (-∞) - (-∞) = unknown
            (Self::PositiveInfinity, _) => Self::PositiveInfinity,
            (_, Self::PositiveInfinity) => Self::NegativeInfinity, // x - (+∞) = -∞
            (Self::NegativeInfinity, _) => Self::NegativeInfinity,
            (_, Self::NegativeInfinity) => Self::PositiveInfinity, // x - (-∞) = +∞
            (Self::Undefined, _) | (_, Self::Undefined) => Self::Undefined,

            // Type mismatch
            _ => Self::Undefined,
        }
    }

    /// Multiplies two abstract values.
    ///
    /// Handles all type combinations with sign analysis.
    /// All results are normalized.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let a = AbstractValue::int_constant(5);
    /// let b = AbstractValue::int_constant(10);
    /// assert_eq!(a.mul(&b), AbstractValue::int_constant(50));
    ///
    /// let a = AbstractValue::int_interval(0, 5);
    /// let b = AbstractValue::int_interval(10, 20);
    /// // Result: [0, 100] (both non-negative)
    /// ```
    pub fn mul(&self, other: &Self) -> Self {
        match (self, other) {
            // Special values first (before zero check to handle 0 * ∞)
            (Self::PositiveInfinity, Self::IntConstant(0))
            | (Self::IntConstant(0), Self::PositiveInfinity) => {
                Self::IntTop // 0 * ∞ = undefined
            }
            (Self::NegativeInfinity, Self::IntConstant(0))
            | (Self::IntConstant(0), Self::NegativeInfinity) => {
                Self::IntTop // 0 * (-∞) = undefined
            }

            // Concrete * Concrete
            (Self::IntConstant(a), Self::IntConstant(b)) => {
                match a.checked_mul(*b) {
                    Some(n) => Self::IntConstant(n),
                    None => {
                        // Overflow: determine sign
                        if (*a > 0 && *b > 0) || (*a < 0 && *b < 0) {
                            Self::PositiveInfinity
                        } else {
                            Self::NegativeInfinity
                        }
                    }
                }
            }

            // Handle zero (after infinity checks)
            (Self::IntConstant(0), _) | (_, Self::IntConstant(0)) => Self::IntConstant(0),

            // Interval * Interval: need to consider all corner cases
            (Self::IntInterval(a_min, a_max), Self::IntInterval(b_min, b_max)) => {
                // Check if interval contains zero
                let a_contains_zero = *a_min <= 0 && *a_max >= 0;
                let b_contains_zero = *b_min <= 0 && *b_max >= 0;

                if a_contains_zero || b_contains_zero {
                    // Complex case: need to consider all corners
                    let corners = [
                        a_min.checked_mul(*b_min),
                        a_min.checked_mul(*b_max),
                        a_max.checked_mul(*b_min),
                        a_max.checked_mul(*b_max),
                    ];

                    let mut min_val: Option<i64> = None;
                    let mut max_val: Option<i64> = None;

                    for corner in corners.iter().flatten() {
                        min_val = min_val.map(|m| m.min(*corner)).or(Some(*corner));
                        max_val = max_val.map(|m| m.max(*corner)).or(Some(*corner));
                    }

                    match (min_val, max_val) {
                        (Some(min), Some(max)) if min == max => Self::IntConstant(min),
                        (Some(min), Some(max)) => Self::IntInterval(min, max).normalize(),
                        _ => Self::IntTop, // Overflow in corners
                    }
                } else {
                    // No zero in either interval - simpler case
                    let corners = [
                        a_min.checked_mul(*b_min),
                        a_min.checked_mul(*b_max),
                        a_max.checked_mul(*b_min),
                        a_max.checked_mul(*b_max),
                    ];

                    let mut min_val: Option<i64> = None;
                    let mut max_val: Option<i64> = None;

                    for corner in corners.iter().flatten() {
                        min_val = min_val.map(|m| m.min(*corner)).or(Some(*corner));
                        max_val = max_val.map(|m| m.max(*corner)).or(Some(*corner));
                    }

                    match (min_val, max_val) {
                        (Some(min), Some(max)) if min == max => Self::IntConstant(min),
                        (Some(min), Some(max)) => Self::IntInterval(min, max).normalize(),
                        _ => Self::IntTop,
                    }
                }
            }

            // Constant * Interval (symmetric)
            (Self::IntConstant(a), Self::IntInterval(b_min, b_max)) => {
                if *a == 0 {
                    Self::IntConstant(0)
                } else {
                    let min_result = b_min.checked_mul(*a);
                    let max_result = b_max.checked_mul(*a);

                    match (min_result, max_result) {
                        (Some(min), Some(max)) => {
                            if *a < 0 {
                                // Negative multiplier reverses bounds
                                if min == max {
                                    Self::IntConstant(min)
                                } else {
                                    Self::IntInterval(max, min).normalize()
                                }
                            } else if min == max {
                                Self::IntConstant(min)
                            } else {
                                Self::IntInterval(min, max).normalize()
                            }
                        }
                        _ => Self::IntTop,
                    }
                }
            }
            (Self::IntInterval(a_min, a_max), Self::IntConstant(b)) => {
                // Symmetric case: implement directly to avoid recursion
                if *b == 0 {
                    Self::IntConstant(0)
                } else {
                    let min_result = a_min.checked_mul(*b);
                    let max_result = a_max.checked_mul(*b);

                    match (min_result, max_result) {
                        (Some(min), Some(max)) => {
                            if *b < 0 {
                                // Negative multiplier reverses bounds
                                if min == max {
                                    Self::IntConstant(min)
                                } else {
                                    Self::IntInterval(max, min).normalize()
                                }
                            } else if min == max {
                                Self::IntConstant(min)
                            } else {
                                Self::IntInterval(min, max).normalize()
                            }
                        }
                        _ => Self::IntTop,
                    }
                }
            }

            // Set * Set
            (Self::IntSet(a_set), Self::IntSet(b_set)) => {
                let mut result_set = HashSet::new();
                for a in a_set {
                    for b in b_set {
                        if let Some(product) = a.checked_mul(*b) {
                            result_set.insert(product);
                        } else {
                            // Overflow in set multiplication - result is unbounded
                            return Self::IntTop;
                        }
                    }
                }
                if result_set.is_empty() {
                    Self::IntTop
                } else {
                    Self::IntSet(result_set).normalize()
                }
            }

            // Constant * Set (symmetric)
            (Self::IntConstant(a), Self::IntSet(b_set)) => {
                if *a == 0 {
                    Self::IntConstant(0)
                } else {
                    let mut result_set = HashSet::new();
                    for b in b_set {
                        if let Some(product) = a.checked_mul(*b) {
                            result_set.insert(product);
                        } else {
                            return Self::IntTop;
                        }
                    }
                    Self::IntSet(result_set).normalize()
                }
            }
            (Self::IntSet(a_set), Self::IntConstant(b)) => {
                // Symmetric case: implement directly to avoid recursion
                if *b == 0 {
                    Self::IntConstant(0)
                } else {
                    let mut result_set = HashSet::new();
                    for a in a_set {
                        if let Some(product) = a.checked_mul(*b) {
                            result_set.insert(product);
                        } else {
                            return Self::IntTop;
                        }
                    }
                    Self::IntSet(result_set).normalize()
                }
            }

            // Interval * Set: convert set to interval bounds
            (Self::IntInterval(a_min, a_max), Self::IntSet(b_set)) => {
                if let (Some(b_min), Some(b_max)) =
                    (b_set.iter().min().copied(), b_set.iter().max().copied())
                {
                    Self::IntInterval(*a_min, *a_max).mul(&Self::IntInterval(b_min, b_max))
                } else {
                    Self::IntTop
                }
            }
            (Self::IntSet(a_set), Self::IntInterval(b_min, b_max)) => {
                // Symmetric case: implement directly to avoid recursion
                if let (Some(a_min), Some(a_max)) =
                    (a_set.iter().min().copied(), a_set.iter().max().copied())
                {
                    Self::IntInterval(a_min, a_max).mul(&Self::IntInterval(*b_min, *b_max))
                } else {
                    Self::IntTop
                }
            }

            // Top values
            (Self::IntTop, _) | (_, Self::IntTop) => Self::IntTop,

            // Special values
            (Self::PositiveInfinity, Self::PositiveInfinity) => Self::PositiveInfinity,
            (Self::NegativeInfinity, Self::NegativeInfinity) => Self::PositiveInfinity, // (-∞) * (-∞) = +∞
            (Self::PositiveInfinity, Self::NegativeInfinity)
            | (Self::NegativeInfinity, Self::PositiveInfinity) => Self::NegativeInfinity,
            (Self::PositiveInfinity, _) | (_, Self::PositiveInfinity) => {
                // Check if other is negative
                if matches!(other, Self::IntConstant(n) if *n < 0)
                    || matches!(self, Self::IntConstant(n) if *n < 0)
                {
                    Self::NegativeInfinity
                } else {
                    Self::PositiveInfinity
                }
            }
            (Self::NegativeInfinity, _) | (_, Self::NegativeInfinity) => {
                // Check if other is negative
                if matches!(other, Self::IntConstant(n) if *n < 0)
                    || matches!(self, Self::IntConstant(n) if *n < 0)
                {
                    Self::PositiveInfinity
                } else {
                    Self::NegativeInfinity
                }
            }
            (Self::Undefined, _) | (_, Self::Undefined) => Self::Undefined,

            // Type mismatch
            _ => Self::Undefined,
        }
    }

    /// Divides two abstract values.
    ///
    /// Handles all type combinations and division by zero.
    /// Division by zero returns `Undefined`.
    /// All results are normalized.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let a = AbstractValue::int_constant(10);
    /// let b = AbstractValue::int_constant(2);
    /// assert_eq!(a.div(&b), Ok(AbstractValue::int_constant(5)));
    ///
    /// let a = AbstractValue::int_constant(10);
    /// let b = AbstractValue::int_constant(0);
    /// assert_eq!(a.div(&b), Err(())); // Division by zero
    /// ```
    #[allow(clippy::result_unit_err)] // Using () for division by zero is idiomatic
    pub fn div(&self, other: &Self) -> Result<Self, ()> {
        match (self, other) {
            // Check for division by zero
            (_, Self::IntConstant(0)) => Err(()),
            (_, Self::IntInterval(b_min, b_max)) if *b_min <= 0 && *b_max >= 0 => {
                // Divisor interval contains zero
                Err(())
            }
            (_, Self::IntSet(b_set)) if b_set.contains(&0) => Err(()),

            // Concrete / Concrete
            (Self::IntConstant(a), Self::IntConstant(b)) => {
                if *b == 0 {
                    Err(())
                } else {
                    match a.checked_div(*b) {
                        Some(n) => Ok(Self::IntConstant(n)),
                        None => {
                            // Division overflow (rare, but possible with i64::MIN / -1)
                            Ok(Self::IntTop)
                        }
                    }
                }
            }

            // Interval / Interval: [a_min, a_max] / [b_min, b_max]
            // Need to consider all corner cases
            (Self::IntInterval(a_min, a_max), Self::IntInterval(b_min, b_max)) => {
                // Check if divisor contains zero (already checked above, but be safe)
                if *b_min <= 0 && *b_max >= 0 {
                    return Err(());
                }

                let corners = [
                    a_min.checked_div(*b_min),
                    a_min.checked_div(*b_max),
                    a_max.checked_div(*b_min),
                    a_max.checked_div(*b_max),
                ];

                let mut min_val: Option<i64> = None;
                let mut max_val: Option<i64> = None;

                for corner in corners.iter().flatten() {
                    min_val = min_val.map(|m| m.min(*corner)).or(Some(*corner));
                    max_val = max_val.map(|m| m.max(*corner)).or(Some(*corner));
                }

                match (min_val, max_val) {
                    (Some(min), Some(max)) if min == max => Ok(Self::IntConstant(min)),
                    (Some(min), Some(max)) => Ok(Self::IntInterval(min, max).normalize()),
                    _ => Ok(Self::IntTop), // Overflow or complex case
                }
            }

            // Constant / Interval
            (Self::IntConstant(a), Self::IntInterval(b_min, b_max)) => {
                if *b_min <= 0 && *b_max >= 0 {
                    Err(())
                } else {
                    // Divide by interval bounds
                    let min_result = a.checked_div(*b_max); // Reverse for division
                    let max_result = a.checked_div(*b_min);

                    match (min_result, max_result) {
                        (Some(min), Some(max)) => {
                            if min == max {
                                Ok(Self::IntConstant(min))
                            } else {
                                Ok(Self::IntInterval(min, max).normalize())
                            }
                        }
                        _ => Ok(Self::IntTop),
                    }
                }
            }
            (Self::IntInterval(a_min, a_max), Self::IntConstant(b)) => {
                if *b == 0 {
                    Err(())
                } else {
                    let min_result = a_min.checked_div(*b);
                    let max_result = a_max.checked_div(*b);

                    match (min_result, max_result) {
                        (Some(min), Some(max)) => {
                            if *b < 0 {
                                // Negative divisor reverses bounds
                                if min == max {
                                    Ok(Self::IntConstant(min))
                                } else {
                                    Ok(Self::IntInterval(max, min).normalize())
                                }
                            } else if min == max {
                                Ok(Self::IntConstant(min))
                            } else {
                                Ok(Self::IntInterval(min, max).normalize())
                            }
                        }
                        _ => Ok(Self::IntTop),
                    }
                }
            }

            // Set / Set
            (Self::IntSet(a_set), Self::IntSet(b_set)) => {
                if b_set.contains(&0) {
                    Err(())
                } else {
                    let mut result_set = HashSet::new();
                    for a in a_set {
                        for b in b_set {
                            if *b != 0
                                && let Some(quotient) = a.checked_div(*b)
                            {
                                result_set.insert(quotient);
                            }
                            // Division overflow is rare, ignore for sets
                        }
                    }
                    if result_set.is_empty() {
                        Ok(Self::IntTop)
                    } else {
                        Ok(Self::IntSet(result_set).normalize())
                    }
                }
            }

            // Constant / Set
            (Self::IntConstant(a), Self::IntSet(b_set)) => {
                if b_set.contains(&0) {
                    Err(())
                } else {
                    let mut result_set = HashSet::new();
                    for b in b_set {
                        if *b != 0
                            && let Some(quotient) = a.checked_div(*b)
                        {
                            result_set.insert(quotient);
                        }
                    }
                    Ok(Self::IntSet(result_set).normalize())
                }
            }
            (Self::IntSet(a_set), Self::IntConstant(b)) => {
                if *b == 0 {
                    Err(())
                } else {
                    let mut result_set = HashSet::new();
                    for a in a_set {
                        if let Some(quotient) = a.checked_div(*b) {
                            result_set.insert(quotient);
                        }
                    }
                    Ok(Self::IntSet(result_set).normalize())
                }
            }

            // Interval / Set: convert set to interval
            (Self::IntInterval(a_min, a_max), Self::IntSet(b_set)) => {
                if b_set.contains(&0) {
                    Err(())
                } else if let (Some(b_min), Some(b_max)) =
                    (b_set.iter().min().copied(), b_set.iter().max().copied())
                {
                    Self::IntInterval(*a_min, *a_max).div(&Self::IntInterval(b_min, b_max))
                } else {
                    Ok(Self::IntTop)
                }
            }
            (Self::IntSet(a_set), Self::IntInterval(b_min, b_max)) => {
                if *b_min <= 0 && *b_max >= 0 {
                    Err(())
                } else if let (Some(a_min), Some(a_max)) =
                    (a_set.iter().min().copied(), a_set.iter().max().copied())
                {
                    Self::IntInterval(a_min, a_max).div(&Self::IntInterval(*b_min, *b_max))
                } else {
                    Ok(Self::IntTop)
                }
            }

            // Top values
            (Self::IntTop, _) | (_, Self::IntTop) => Ok(Self::IntTop),

            // Special values
            (Self::PositiveInfinity, Self::PositiveInfinity)
            | (Self::NegativeInfinity, Self::NegativeInfinity) => {
                Ok(Self::IntTop) // ∞ / ∞ = undefined
            }
            (Self::PositiveInfinity, Self::NegativeInfinity)
            | (Self::NegativeInfinity, Self::PositiveInfinity) => {
                Ok(Self::IntTop) // ∞ / (-∞) = undefined
            }
            (Self::PositiveInfinity, _) => {
                // Check sign of divisor
                if matches!(other, Self::IntConstant(n) if *n < 0) {
                    Ok(Self::NegativeInfinity)
                } else {
                    Ok(Self::PositiveInfinity)
                }
            }
            (Self::NegativeInfinity, _) => {
                // Check sign of divisor
                if matches!(other, Self::IntConstant(n) if *n < 0) {
                    Ok(Self::PositiveInfinity) // Negative / negative = positive
                } else {
                    Ok(Self::NegativeInfinity) // Negative / positive = negative
                }
            }
            (_, Self::PositiveInfinity) | (_, Self::NegativeInfinity) => {
                Ok(Self::IntConstant(0)) // x / ∞ = 0
            }
            (Self::Undefined, _) | (_, Self::Undefined) => Ok(Self::Undefined),

            // Type mismatch
            _ => Ok(Self::Undefined),
        }
    }

    /// Joins two abstract values, computing the least upper bound (⊔).
    ///
    /// The join operation combines two abstract values into a more general abstraction
    /// that represents all values in either operand. All results are normalized.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let a = AbstractValue::int_constant(3);
    /// let b = AbstractValue::int_constant(5);
    /// assert_eq!(a.join(&b), AbstractValue::int_interval(3, 5));
    ///
    /// let a = AbstractValue::int_constant(5);
    /// let b = AbstractValue::int_constant(5);
    /// assert_eq!(a.join(&b), AbstractValue::int_constant(5)); // Normalized
    /// ```
    pub fn join(&self, other: &Self) -> Self {
        match (self, other) {
            // === Integer Operations ===

            // Constant ⊔ Constant
            (Self::IntConstant(a), Self::IntConstant(b)) => {
                if a == b {
                    Self::IntConstant(*a)
                } else {
                    Self::IntInterval((*a).min(*b), (*a).max(*b)).normalize()
                }
            }

            // Constant ⊔ Interval
            (Self::IntConstant(a), Self::IntInterval(b_min, b_max)) => {
                Self::IntInterval((*a).min(*b_min), (*a).max(*b_max)).normalize()
            }
            (Self::IntInterval(a_min, a_max), Self::IntConstant(b)) => {
                Self::IntInterval((*a_min).min(*b), (*a_max).max(*b)).normalize()
            }

            // Interval ⊔ Interval
            (Self::IntInterval(a_min, a_max), Self::IntInterval(b_min, b_max)) => {
                Self::IntInterval((*a_min).min(*b_min), (*a_max).max(*b_max)).normalize()
            }

            // Constant ⊔ Set
            (Self::IntConstant(a), Self::IntSet(b_set)) => {
                let mut result_set = b_set.clone();
                result_set.insert(*a);
                Self::IntSet(result_set).normalize()
            }
            (Self::IntSet(a_set), Self::IntConstant(b)) => {
                let mut result_set = a_set.clone();
                result_set.insert(*b);
                Self::IntSet(result_set).normalize()
            }

            // Interval ⊔ Set: Convert interval to set and join
            (Self::IntInterval(a_min, a_max), Self::IntSet(b_set)) => {
                let mut result_set = b_set.clone();
                // Add all integers in interval to set
                for i in *a_min..=*a_max {
                    result_set.insert(i);
                }
                Self::IntSet(result_set).normalize()
            }
            (Self::IntSet(a_set), Self::IntInterval(b_min, b_max)) => {
                let mut result_set = a_set.clone();
                // Add all integers in interval to set
                for i in *b_min..=*b_max {
                    result_set.insert(i);
                }
                Self::IntSet(result_set).normalize()
            }

            // Set ⊔ Set
            (Self::IntSet(a_set), Self::IntSet(b_set)) => {
                let result_set: HashSet<i64> = a_set.union(b_set).copied().collect();
                Self::IntSet(result_set).normalize()
            }

            // Top absorbs all
            (Self::IntTop, _) | (_, Self::IntTop) => Self::IntTop,
            (Self::PositiveInfinity, _)
            | (_, Self::PositiveInfinity)
            | (Self::NegativeInfinity, _)
            | (_, Self::NegativeInfinity) => Self::IntTop,

            // === Boolean Operations ===

            // Constant ⊔ Constant
            (Self::BoolConstant(a), Self::BoolConstant(b)) => {
                if a == b {
                    Self::BoolConstant(*a)
                } else {
                    Self::BoolSet([true, false].into_iter().collect())
                }
            }

            // Constant ⊔ Set
            (Self::BoolConstant(a), Self::BoolSet(b_set)) => {
                let mut result_set = b_set.clone();
                result_set.insert(*a);
                Self::BoolSet(result_set).normalize()
            }
            (Self::BoolSet(a_set), Self::BoolConstant(b)) => {
                let mut result_set = a_set.clone();
                result_set.insert(*b);
                Self::BoolSet(result_set).normalize()
            }

            // Set ⊔ Set
            (Self::BoolSet(a_set), Self::BoolSet(b_set)) => {
                let result_set: HashSet<bool> = a_set.union(b_set).copied().collect();
                Self::BoolSet(result_set).normalize()
            }

            // === Symbol Operations ===

            // Constant ⊔ Constant
            (Self::SymbolConstant(a), Self::SymbolConstant(b)) => {
                if a == b {
                    Self::SymbolConstant(a.clone())
                } else {
                    let mut result_set = HashSet::new();
                    result_set.insert(a.clone());
                    result_set.insert(b.clone());
                    Self::SymbolSet(result_set).normalize()
                }
            }

            // Constant ⊔ Set
            (Self::SymbolConstant(a), Self::SymbolSet(b_set)) => {
                let mut result_set = b_set.clone();
                result_set.insert(a.clone());
                Self::SymbolSet(result_set).normalize()
            }
            (Self::SymbolSet(a_set), Self::SymbolConstant(b)) => {
                let mut result_set = a_set.clone();
                result_set.insert(b.clone());
                Self::SymbolSet(result_set).normalize()
            }

            // Set ⊔ Set
            (Self::SymbolSet(a_set), Self::SymbolSet(b_set)) => {
                let result_set: HashSet<String> = a_set.union(b_set).cloned().collect();
                Self::SymbolSet(result_set).normalize()
            }

            // SymbolTop absorbs all symbols
            (Self::SymbolTop, _) | (_, Self::SymbolTop) => Self::SymbolTop,

            // === Special Values ===
            (Self::Undefined, _) | (_, Self::Undefined) => Self::Undefined,

            // Type mismatch
            _ => Self::Undefined,
        }
    }

    /// Meets two abstract values, computing the greatest lower bound (⊓).
    ///
    /// The meet operation computes the intersection of two abstract values.
    /// Returns `Undefined` (representing ⊥, bottom) if the intersection is empty.
    /// All results are normalized.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let a = AbstractValue::int_interval(0, 10);
    /// let b = AbstractValue::int_interval(5, 15);
    /// assert_eq!(a.meet(&b), AbstractValue::int_interval(5, 10));
    ///
    /// let a = AbstractValue::int_interval(0, 5);
    /// let b = AbstractValue::int_interval(10, 15);
    /// assert_eq!(a.meet(&b), AbstractValue::Undefined); // Empty intersection
    /// ```
    pub fn meet(&self, other: &Self) -> Self {
        match (self, other) {
            // === Integer Operations ===

            // Constant ⊓ Constant
            (Self::IntConstant(a), Self::IntConstant(b)) => {
                if a == b {
                    Self::IntConstant(*a)
                } else {
                    Self::Undefined // Empty intersection
                }
            }

            // Constant ⊓ Interval
            (Self::IntConstant(a), Self::IntInterval(b_min, b_max)) => {
                if *b_min <= *a && *a <= *b_max {
                    Self::IntConstant(*a)
                } else {
                    Self::Undefined
                }
            }
            (Self::IntInterval(a_min, a_max), Self::IntConstant(b)) => {
                if *a_min <= *b && *b <= *a_max {
                    Self::IntConstant(*b)
                } else {
                    Self::Undefined
                }
            }

            // Interval ⊓ Interval
            (Self::IntInterval(a_min, a_max), Self::IntInterval(b_min, b_max)) => {
                let meet_min = (*a_min).max(*b_min);
                let meet_max = (*a_max).min(*b_max);
                if meet_min <= meet_max {
                    Self::IntInterval(meet_min, meet_max).normalize()
                } else {
                    Self::Undefined // Empty intersection
                }
            }

            // Constant ⊓ Set
            (Self::IntConstant(a), Self::IntSet(b_set)) => {
                if b_set.contains(a) {
                    Self::IntConstant(*a)
                } else {
                    Self::Undefined
                }
            }
            (Self::IntSet(a_set), Self::IntConstant(b)) => {
                if a_set.contains(b) {
                    Self::IntConstant(*b)
                } else {
                    Self::Undefined
                }
            }

            // Interval ⊓ Set: Intersect set with interval
            (Self::IntInterval(a_min, a_max), Self::IntSet(b_set)) => {
                let intersection: HashSet<i64> = b_set
                    .iter()
                    .filter(|x| *a_min <= **x && **x <= *a_max)
                    .copied()
                    .collect();
                if intersection.is_empty() {
                    Self::Undefined
                } else {
                    Self::IntSet(intersection).normalize()
                }
            }
            (Self::IntSet(a_set), Self::IntInterval(b_min, b_max)) => {
                let intersection: HashSet<i64> = a_set
                    .iter()
                    .filter(|x| *b_min <= **x && **x <= *b_max)
                    .copied()
                    .collect();
                if intersection.is_empty() {
                    Self::Undefined
                } else {
                    Self::IntSet(intersection).normalize()
                }
            }

            // Set ⊓ Set
            (Self::IntSet(a_set), Self::IntSet(b_set)) => {
                let intersection: HashSet<i64> = a_set.intersection(b_set).copied().collect();
                if intersection.is_empty() {
                    Self::Undefined
                } else {
                    Self::IntSet(intersection).normalize()
                }
            }

            // Top ⊓ X = X (top is identity for meet)
            (Self::IntTop, x) | (x, Self::IntTop) => x.clone(),

            // Infinity operations
            (Self::PositiveInfinity, Self::PositiveInfinity) => Self::PositiveInfinity,
            (Self::NegativeInfinity, Self::NegativeInfinity) => Self::NegativeInfinity,
            (Self::PositiveInfinity, _)
            | (_, Self::PositiveInfinity)
            | (Self::NegativeInfinity, _)
            | (_, Self::NegativeInfinity) => Self::Undefined,

            // === Boolean Operations ===

            // Constant ⊓ Constant
            (Self::BoolConstant(a), Self::BoolConstant(b)) => {
                if a == b {
                    Self::BoolConstant(*a)
                } else {
                    Self::Undefined
                }
            }

            // Constant ⊓ Set
            (Self::BoolConstant(a), Self::BoolSet(b_set)) => {
                if b_set.contains(a) {
                    Self::BoolConstant(*a)
                } else {
                    Self::Undefined
                }
            }
            (Self::BoolSet(a_set), Self::BoolConstant(b)) => {
                if a_set.contains(b) {
                    Self::BoolConstant(*b)
                } else {
                    Self::Undefined
                }
            }

            // Set ⊓ Set
            (Self::BoolSet(a_set), Self::BoolSet(b_set)) => {
                let intersection: HashSet<bool> = a_set.intersection(b_set).copied().collect();
                if intersection.is_empty() {
                    Self::Undefined
                } else {
                    Self::BoolSet(intersection).normalize()
                }
            }

            // === Symbol Operations ===

            // Constant ⊓ Constant
            (Self::SymbolConstant(a), Self::SymbolConstant(b)) => {
                if a == b {
                    Self::SymbolConstant(a.clone())
                } else {
                    Self::Undefined
                }
            }

            // Constant ⊓ Set
            (Self::SymbolConstant(a), Self::SymbolSet(b_set)) => {
                if b_set.contains(a) {
                    Self::SymbolConstant(a.clone())
                } else {
                    Self::Undefined
                }
            }
            (Self::SymbolSet(a_set), Self::SymbolConstant(b)) => {
                if a_set.contains(b) {
                    Self::SymbolConstant(b.clone())
                } else {
                    Self::Undefined
                }
            }

            // Set ⊓ Set
            (Self::SymbolSet(a_set), Self::SymbolSet(b_set)) => {
                let intersection: HashSet<String> = a_set.intersection(b_set).cloned().collect();
                if intersection.is_empty() {
                    Self::Undefined
                } else {
                    Self::SymbolSet(intersection).normalize()
                }
            }

            // SymbolTop ⊓ X = X
            (Self::SymbolTop, x) | (x, Self::SymbolTop) => x.clone(),

            // === Special Values ===
            (Self::Undefined, _) | (_, Self::Undefined) => Self::Undefined,

            // Type mismatch
            _ => Self::Undefined,
        }
    }

    /// Widens an abstract value to ensure fixpoint convergence (∇).
    ///
    /// Widening is used in abstract interpretation to ensure that fixpoint
    /// iterations converge. It may extend intervals to infinity if they grow
    /// beyond their previous bounds.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let a = AbstractValue::int_interval(0, 10);
    /// let b = AbstractValue::int_interval(5, 20); // Extends upper bound
    /// // Result should extend to infinity if needed for convergence
    /// ```
    pub fn widen(&self, other: &Self) -> Self {
        match (self, other) {
            // === Integer Operations ===

            // Interval ∇ Interval
            (Self::IntInterval(a_min, a_max), Self::IntInterval(b_min, b_max)) => {
                let result_min = (*a_min).min(*b_min);
                let result_max = (*a_max).max(*b_max);

                // If new interval extends beyond old bounds, extend to infinity
                if *b_min < *a_min {
                    // Lower bound decreased: extend to negative infinity
                    Self::IntTop // Represented as IntTop when unbounded
                } else if *b_max > *a_max {
                    // Upper bound increased: extend to positive infinity
                    Self::IntTop // Represented as IntTop when unbounded
                } else {
                    // No extension needed: just take union
                    Self::IntInterval(result_min, result_max).normalize()
                }
            }

            // Set ∇ Set: Convert to interval if union is large
            (Self::IntSet(a_set), Self::IntSet(b_set)) => {
                let union: HashSet<i64> = a_set.union(b_set).copied().collect();
                if union.is_empty() {
                    Self::Undefined
                } else if union.len() > 100 {
                    // Threshold: if union is large, convert to interval
                    let min_val = *union.iter().min().unwrap();
                    let max_val = *union.iter().max().unwrap();
                    Self::IntInterval(min_val, max_val).normalize()
                } else {
                    Self::IntSet(union).normalize()
                }
            }

            // For constants, intervals, sets with each other: convert to appropriate type first
            (Self::IntConstant(a), Self::IntConstant(b)) => {
                if a == b {
                    Self::IntConstant(*a)
                } else {
                    Self::IntInterval((*a).min(*b), (*a).max(*b)).normalize()
                }
            }

            // Convert constants to intervals for widening
            (Self::IntConstant(a), Self::IntInterval(b_min, b_max)) => {
                let min_val = (*a).min(*b_min);
                let max_val = (*a).max(*b_max);
                if *b_min < *a || *b_max > *a {
                    // Extended beyond constant
                    Self::IntTop
                } else {
                    Self::IntInterval(min_val, max_val).normalize()
                }
            }
            (Self::IntInterval(a_min, a_max), Self::IntConstant(b)) => {
                let min_val = (*a_min).min(*b);
                let max_val = (*a_max).max(*b);
                if *b < *a_min || *b > *a_max {
                    // Extended beyond interval
                    Self::IntTop
                } else {
                    Self::IntInterval(min_val, max_val).normalize()
                }
            }

            // Set with interval/constant: convert to appropriate type
            (Self::IntSet(_), _) => {
                // Convert set to interval if needed, then widen
                let self_interval = self.to_interval_approximation();
                self_interval.widen(other)
            }
            (_, Self::IntSet(_)) => {
                let other_interval = other.to_interval_approximation();
                self.widen(&other_interval)
            }

            // Top operations
            (Self::IntTop, _) | (_, Self::IntTop) => Self::IntTop,
            (Self::PositiveInfinity, _)
            | (_, Self::PositiveInfinity)
            | (Self::NegativeInfinity, _)
            | (_, Self::NegativeInfinity) => Self::IntTop,

            // === Boolean Operations ===
            // For finite domains, widening is same as join
            (Self::BoolConstant(_), _) | (Self::BoolSet(_), _) => self.join(other),

            // === Symbol Operations ===
            // For finite domains, widening is same as join
            (Self::SymbolConstant(_), _) | (Self::SymbolSet(_), _) | (Self::SymbolTop, _) => {
                self.join(other)
            }

            // === Special Values ===
            (Self::Undefined, _) | (_, Self::Undefined) => Self::Undefined,

            // Type mismatch
            _ => Self::Undefined,
        }
    }

    /// Helper to convert a set to an interval approximation for widening.
    fn to_interval_approximation(&self) -> Self {
        match self {
            Self::IntSet(set) => {
                if set.is_empty() {
                    Self::Undefined
                } else {
                    let min_val = *set.iter().min().unwrap();
                    let max_val = *set.iter().max().unwrap();
                    Self::IntInterval(min_val, max_val)
                }
            }
            _ => self.clone(),
        }
    }

    /// Narrows an abstract value to refine approximations (Δ).
    ///
    /// Narrowing is used to refine abstract values after widening.
    /// It computes the intersection, similar to meet, but is used in
    /// a narrowing sequence after widening.
    ///
    /// # Examples
    /// ```
    /// use mununu_core::abstraction::AbstractValue;
    /// let a = AbstractValue::int_interval(0, 20);
    /// let b = AbstractValue::int_interval(5, 15);
    /// assert_eq!(a.narrow(&b), AbstractValue::int_interval(5, 15));
    /// ```
    pub fn narrow(&self, other: &Self) -> Self {
        match (self, other) {
            // === Integer Operations ===

            // Interval Δ Interval
            (Self::IntInterval(a_min, a_max), Self::IntInterval(b_min, b_max)) => {
                let narrow_min = (*a_min).max(*b_min);
                let narrow_max = (*a_max).min(*b_max);
                if narrow_min <= narrow_max {
                    Self::IntInterval(narrow_min, narrow_max).normalize()
                } else {
                    Self::Undefined
                }
            }

            // Set Δ Set: Intersection (same as meet)
            (Self::IntSet(a_set), Self::IntSet(b_set)) => {
                let intersection: HashSet<i64> = a_set.intersection(b_set).copied().collect();
                if intersection.is_empty() {
                    Self::Undefined
                } else {
                    Self::IntSet(intersection).normalize()
                }
            }

            // For constants and mixed types, use meet
            (_, _) => self.meet(other),
        }
    }
}

impl fmt::Display for AbstractValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Concrete values
            Self::IntConstant(n) => write!(f, "{}", n),
            Self::BoolConstant(b) => write!(f, "{}", b),
            Self::SymbolConstant(s) => write!(f, "\"{}\"", s),

            // Abstract integer values
            Self::IntInterval(min, max) => {
                if min == max {
                    write!(f, "{}", min)
                } else {
                    write!(f, "[{}, {}]", min, max)
                }
            }
            Self::IntSet(set) => {
                if set.len() == 1 {
                    write!(f, "{}", set.iter().next().unwrap())
                } else {
                    let mut sorted: Vec<_> = set.iter().collect();
                    sorted.sort();
                    write!(
                        f,
                        "{{{}}}",
                        sorted
                            .iter()
                            .map(|n| n.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Self::IntTop => write!(f, "IntTop"),

            // Abstract boolean values
            Self::BoolSet(set) => {
                if set.len() == 1 {
                    write!(f, "{}", set.iter().next().unwrap())
                } else if set.len() == 2 {
                    write!(f, "{{{{true, false}}}}")
                } else {
                    write!(f, "{{{{}}}}")
                }
            }

            // Abstract symbol values
            Self::SymbolSet(set) => {
                if set.len() == 1 {
                    write!(f, "\"{}\"", set.iter().next().unwrap())
                } else {
                    let mut sorted: Vec<_> = set.iter().collect();
                    sorted.sort();
                    write!(
                        f,
                        "{{{}}}",
                        sorted
                            .iter()
                            .map(|s| format!("\"{}\"", s))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Self::SymbolTop => write!(f, "SymbolTop"),

            // Special values
            Self::PositiveInfinity => write!(f, "+∞"),
            Self::NegativeInfinity => write!(f, "-∞"),
            Self::Undefined => write!(f, "undefined"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Constructor Tests ===

    #[test]
    fn test_int_constant() {
        let val = AbstractValue::int_constant(42);
        assert!(val.is_int_constant());
        assert_eq!(val.as_int_constant(), Some(42));
        assert_eq!(format!("{}", val), "42");
    }

    #[test]
    fn test_bool_constant() {
        let val_true = AbstractValue::bool_constant(true);
        assert!(val_true.is_bool_constant());
        assert_eq!(val_true.as_bool_constant(), Some(true));
        assert_eq!(format!("{}", val_true), "true");

        let val_false = AbstractValue::bool_constant(false);
        assert!(val_false.is_bool_constant());
        assert_eq!(val_false.as_bool_constant(), Some(false));
        assert_eq!(format!("{}", val_false), "false");
    }

    #[test]
    fn test_symbol_constant() {
        let val = AbstractValue::symbol_constant("pending".to_string());
        assert!(val.is_symbol_constant());
        assert_eq!(val.as_symbol_constant(), Some(&"pending".to_string()));
        assert_eq!(format!("{}", val), "\"pending\"");
    }

    #[test]
    fn test_int_interval() {
        let val = AbstractValue::int_interval(0, 10);
        assert_eq!(val.as_int_interval(), Some((0, 10)));
        assert_eq!(format!("{}", val), "[0, 10]");
    }

    #[test]
    fn test_int_interval_singleton() {
        let val = AbstractValue::int_interval(5, 5);
        assert_eq!(val.as_int_interval(), Some((5, 5)));
        assert_eq!(format!("{}", val), "5");
    }

    #[test]
    #[should_panic(expected = "interval min")]
    fn test_int_interval_invalid() {
        AbstractValue::int_interval(10, 0); // min > max should panic
    }

    #[test]
    fn test_int_set() {
        let val = AbstractValue::int_set(vec![0, 1, 2]);
        assert!(val.as_int_set().is_some());
        let set = val.as_int_set().unwrap();
        assert_eq!(set.len(), 3);
        assert!(set.contains(&0));
        assert!(set.contains(&1));
        assert!(set.contains(&2));
    }

    #[test]
    fn test_int_set_singleton() {
        let val = AbstractValue::int_set(vec![42]);
        assert!(val.as_int_set().is_some());
        let set = val.as_int_set().unwrap();
        assert_eq!(set.len(), 1);
        assert!(set.contains(&42));
        assert_eq!(format!("{}", val), "42");
    }

    #[test]
    fn test_int_top() {
        let val = AbstractValue::int_top();
        assert!(val.is_int_top());
        assert_eq!(format!("{}", val), "IntTop");
    }

    #[test]
    fn test_bool_set() {
        let val = AbstractValue::bool_set(vec![true]);
        assert!(val.as_bool_set().is_some());
        let set = val.as_bool_set().unwrap();
        assert_eq!(set.len(), 1);
        assert!(set.contains(&true));
        assert_eq!(format!("{}", val), "true");
    }

    #[test]
    fn test_bool_set_top() {
        let val = AbstractValue::bool_set(vec![true, false]);
        assert!(val.as_bool_set().is_some());
        let set = val.as_bool_set().unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains(&true));
        assert!(set.contains(&false));
        assert_eq!(format!("{}", val), "{{true, false}}");
    }

    #[test]
    fn test_symbol_set() {
        let val = AbstractValue::symbol_set(vec!["pending".to_string(), "active".to_string()]);
        assert!(val.as_symbol_set().is_some());
        let set = val.as_symbol_set().unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains(&"pending".to_string()));
        assert!(set.contains(&"active".to_string()));
    }

    #[test]
    fn test_symbol_set_singleton() {
        let val = AbstractValue::symbol_set(vec!["pending".to_string()]);
        assert!(val.as_symbol_set().is_some());
        let set = val.as_symbol_set().unwrap();
        assert_eq!(set.len(), 1);
        assert!(set.contains(&"pending".to_string()));
        assert_eq!(format!("{}", val), "\"pending\"");
    }

    #[test]
    fn test_symbol_top() {
        let val = AbstractValue::symbol_top();
        assert!(val.is_symbol_top());
        assert_eq!(format!("{}", val), "SymbolTop");
    }

    #[test]
    fn test_positive_infinity() {
        let val = AbstractValue::PositiveInfinity;
        assert!(val.is_positive_infinity());
        assert_eq!(format!("{}", val), "+∞");
    }

    #[test]
    fn test_negative_infinity() {
        let val = AbstractValue::NegativeInfinity;
        assert!(val.is_negative_infinity());
        assert_eq!(format!("{}", val), "-∞");
    }

    #[test]
    fn test_undefined() {
        let val = AbstractValue::Undefined;
        assert!(val.is_undefined());
        assert_eq!(format!("{}", val), "undefined");
    }

    // === Type Checking Tests ===

    #[test]
    fn test_is_integer() {
        assert!(AbstractValue::int_constant(5).is_integer());
        assert!(AbstractValue::int_interval(0, 10).is_integer());
        assert!(AbstractValue::int_set(vec![1, 2, 3]).is_integer());
        assert!(AbstractValue::int_top().is_integer());
        assert!(AbstractValue::PositiveInfinity.is_integer());
        assert!(AbstractValue::NegativeInfinity.is_integer());
        assert!(!AbstractValue::bool_constant(true).is_integer());
        assert!(!AbstractValue::symbol_constant("x".to_string()).is_integer());
    }

    #[test]
    fn test_is_boolean() {
        assert!(AbstractValue::bool_constant(true).is_boolean());
        assert!(AbstractValue::bool_set(vec![true, false]).is_boolean());
        assert!(!AbstractValue::int_constant(5).is_boolean());
        assert!(!AbstractValue::symbol_constant("x".to_string()).is_boolean());
    }

    #[test]
    fn test_is_symbol() {
        assert!(AbstractValue::symbol_constant("x".to_string()).is_symbol());
        assert!(AbstractValue::symbol_set(vec!["a".to_string()]).is_symbol());
        assert!(AbstractValue::symbol_top().is_symbol());
        assert!(!AbstractValue::int_constant(5).is_symbol());
        assert!(!AbstractValue::bool_constant(true).is_symbol());
    }

    // === Equality Tests ===

    #[test]
    fn test_equality_int_constant() {
        let val1 = AbstractValue::int_constant(5);
        let val2 = AbstractValue::int_constant(5);
        let val3 = AbstractValue::int_constant(10);
        assert_eq!(val1, val2);
        assert_ne!(val1, val3);
    }

    #[test]
    fn test_equality_bool_constant() {
        let val1 = AbstractValue::bool_constant(true);
        let val2 = AbstractValue::bool_constant(true);
        let val3 = AbstractValue::bool_constant(false);
        assert_eq!(val1, val2);
        assert_ne!(val1, val3);
    }

    #[test]
    fn test_equality_symbol_constant() {
        let val1 = AbstractValue::symbol_constant("pending".to_string());
        let val2 = AbstractValue::symbol_constant("pending".to_string());
        let val3 = AbstractValue::symbol_constant("active".to_string());
        assert_eq!(val1, val2);
        assert_ne!(val1, val3);
    }

    #[test]
    fn test_equality_int_interval() {
        let val1 = AbstractValue::int_interval(0, 10);
        let val2 = AbstractValue::int_interval(0, 10);
        let val3 = AbstractValue::int_interval(0, 5);
        assert_eq!(val1, val2);
        assert_ne!(val1, val3);
    }

    #[test]
    fn test_equality_int_set() {
        let val1 = AbstractValue::int_set(vec![0, 1, 2]);
        let val2 = AbstractValue::int_set(vec![2, 1, 0]); // Same elements, different order
        let val3 = AbstractValue::int_set(vec![0, 1, 3]);
        assert_eq!(val1, val2); // Sets are equal regardless of order
        assert_ne!(val1, val3);
    }

    #[test]
    fn test_equality_bool_set() {
        let val1 = AbstractValue::bool_set(vec![true, false]);
        let val2 = AbstractValue::bool_set(vec![false, true]); // Same elements, different order
        assert_eq!(val1, val2); // Sets are equal regardless of order
    }

    #[test]
    fn test_equality_symbol_set() {
        let val1 = AbstractValue::symbol_set(vec!["a".to_string(), "b".to_string()]);
        let val2 = AbstractValue::symbol_set(vec!["b".to_string(), "a".to_string()]);
        assert_eq!(val1, val2); // Sets are equal regardless of order
    }

    // Note: Hash test removed because HashSet doesn't implement Hash.
    // Hash can be implemented manually if needed in the future.

    // === Display Tests ===

    #[test]
    fn test_display_int_constant() {
        assert_eq!(format!("{}", AbstractValue::int_constant(42)), "42");
        assert_eq!(format!("{}", AbstractValue::int_constant(-5)), "-5");
    }

    #[test]
    fn test_display_bool_constant() {
        assert_eq!(format!("{}", AbstractValue::bool_constant(true)), "true");
        assert_eq!(format!("{}", AbstractValue::bool_constant(false)), "false");
    }

    #[test]
    fn test_display_symbol_constant() {
        assert_eq!(
            format!("{}", AbstractValue::symbol_constant("pending".to_string())),
            "\"pending\""
        );
    }

    #[test]
    fn test_display_int_interval() {
        assert_eq!(format!("{}", AbstractValue::int_interval(0, 10)), "[0, 10]");
        assert_eq!(format!("{}", AbstractValue::int_interval(5, 5)), "5");
    }

    #[test]
    fn test_display_int_set() {
        let val = AbstractValue::int_set(vec![2, 1, 0]);
        let display = format!("{}", val);
        // Order may vary, but should contain all elements
        assert!(display.contains("0"));
        assert!(display.contains("1"));
        assert!(display.contains("2"));
    }

    #[test]
    fn test_display_bool_set() {
        assert_eq!(format!("{}", AbstractValue::bool_set(vec![true])), "true");
        assert_eq!(format!("{}", AbstractValue::bool_set(vec![false])), "false");
        assert_eq!(
            format!("{}", AbstractValue::bool_set(vec![true, false])),
            "{{true, false}}"
        );
    }

    #[test]
    fn test_display_symbol_set() {
        let val = AbstractValue::symbol_set(vec!["b".to_string(), "a".to_string()]);
        let display = format!("{}", val);
        // Should contain both symbols (order may vary)
        assert!(display.contains("a"));
        assert!(display.contains("b"));
    }

    #[test]
    fn test_display_special_values() {
        assert_eq!(format!("{}", AbstractValue::PositiveInfinity), "+∞");
        assert_eq!(format!("{}", AbstractValue::NegativeInfinity), "-∞");
        assert_eq!(format!("{}", AbstractValue::Undefined), "undefined");
    }

    // === Normalization Tests ===

    #[test]
    fn test_normalize_int_interval_singleton() {
        // IntInterval(n, n) → IntConstant(n)
        let val = AbstractValue::int_interval(5, 5);
        let normalized = val.normalize();
        assert_eq!(normalized, AbstractValue::int_constant(5));
        assert!(normalized.is_int_constant());
    }

    #[test]
    fn test_normalize_int_interval_non_singleton() {
        // IntInterval(min, max) where min != max should remain unchanged
        let val = AbstractValue::int_interval(0, 10);
        let normalized = val.normalize();
        assert_eq!(normalized, AbstractValue::int_interval(0, 10));
    }

    #[test]
    fn test_normalize_int_set_singleton() {
        // IntSet({n}) → IntConstant(n)
        let val = AbstractValue::int_set(vec![42]);
        let normalized = val.normalize();
        assert_eq!(normalized, AbstractValue::int_constant(42));
        assert!(normalized.is_int_constant());
    }

    #[test]
    fn test_normalize_int_set_multiple() {
        // IntSet with multiple elements should remain unchanged
        let val = AbstractValue::int_set(vec![1, 2, 3]);
        let normalized = val.normalize();
        // Should remain as IntSet
        assert!(matches!(normalized, AbstractValue::IntSet(_)));
        if let AbstractValue::IntSet(set) = normalized {
            assert_eq!(set.len(), 3);
            assert!(set.contains(&1));
            assert!(set.contains(&2));
            assert!(set.contains(&3));
        }
    }

    #[test]
    fn test_normalize_bool_set_singleton_true() {
        // BoolSet({true}) → BoolConstant(true)
        let val = AbstractValue::bool_set(vec![true]);
        let normalized = val.normalize();
        assert_eq!(normalized, AbstractValue::bool_constant(true));
        assert!(normalized.is_bool_constant());
    }

    #[test]
    fn test_normalize_bool_set_singleton_false() {
        // BoolSet({false}) → BoolConstant(false)
        let val = AbstractValue::bool_set(vec![false]);
        let normalized = val.normalize();
        assert_eq!(normalized, AbstractValue::bool_constant(false));
        assert!(normalized.is_bool_constant());
    }

    #[test]
    fn test_normalize_bool_set_top() {
        // BoolSet({true, false}) should remain unchanged (top element)
        let val = AbstractValue::bool_set(vec![true, false]);
        let normalized = val.normalize();
        // Should remain as BoolSet
        assert!(matches!(normalized, AbstractValue::BoolSet(_)));
        if let AbstractValue::BoolSet(set) = normalized {
            assert_eq!(set.len(), 2);
            assert!(set.contains(&true));
            assert!(set.contains(&false));
        }
    }

    #[test]
    fn test_normalize_symbol_set_singleton() {
        // SymbolSet({s}) → SymbolConstant(s)
        let val = AbstractValue::symbol_set(vec!["pending".to_string()]);
        let normalized = val.normalize();
        assert_eq!(
            normalized,
            AbstractValue::symbol_constant("pending".to_string())
        );
        assert!(normalized.is_symbol_constant());
    }

    #[test]
    fn test_normalize_symbol_set_multiple() {
        // SymbolSet with multiple elements should remain unchanged
        let val = AbstractValue::symbol_set(vec!["pending".to_string(), "active".to_string()]);
        let normalized = val.normalize();
        // Should remain as SymbolSet
        assert!(matches!(normalized, AbstractValue::SymbolSet(_)));
        if let AbstractValue::SymbolSet(set) = normalized {
            assert_eq!(set.len(), 2);
            assert!(set.contains("pending"));
            assert!(set.contains("active"));
        }
    }

    #[test]
    fn test_normalize_concrete_values() {
        // Concrete values should remain unchanged (already normalized)
        let int_val = AbstractValue::int_constant(5);
        assert_eq!(int_val.clone().normalize(), int_val);

        let bool_val = AbstractValue::bool_constant(true);
        assert_eq!(bool_val.clone().normalize(), bool_val);

        let symbol_val = AbstractValue::symbol_constant("test".to_string());
        assert_eq!(symbol_val.clone().normalize(), symbol_val);
    }

    #[test]
    fn test_normalize_top_values() {
        // Top values should remain unchanged
        let int_top = AbstractValue::int_top();
        assert_eq!(int_top.clone().normalize(), int_top);

        let symbol_top = AbstractValue::symbol_top();
        assert_eq!(symbol_top.clone().normalize(), symbol_top);
    }

    #[test]
    fn test_normalize_special_values() {
        // Special values should remain unchanged
        let pos_inf = AbstractValue::PositiveInfinity;
        assert_eq!(pos_inf.clone().normalize(), pos_inf);

        let neg_inf = AbstractValue::NegativeInfinity;
        assert_eq!(neg_inf.clone().normalize(), neg_inf);

        let undefined = AbstractValue::Undefined;
        assert_eq!(undefined.clone().normalize(), undefined);
    }

    #[test]
    fn test_normalize_idempotent() {
        // Normalization should be idempotent: normalize(normalize(x)) == normalize(x)
        let test_cases = vec![
            AbstractValue::int_interval(5, 5),
            AbstractValue::int_set(vec![42]),
            AbstractValue::bool_set(vec![true]),
            AbstractValue::symbol_set(vec!["test".to_string()]),
            AbstractValue::int_constant(10),
            AbstractValue::bool_constant(false),
            AbstractValue::symbol_constant("value".to_string()),
            AbstractValue::int_interval(0, 10),
            AbstractValue::int_set(vec![1, 2, 3]),
            AbstractValue::bool_set(vec![true, false]),
            AbstractValue::symbol_set(vec!["a".to_string(), "b".to_string()]),
            AbstractValue::int_top(),
            AbstractValue::symbol_top(),
            AbstractValue::PositiveInfinity,
            AbstractValue::NegativeInfinity,
            AbstractValue::Undefined,
        ];

        for val in test_cases {
            let normalized_once = val.clone().normalize();
            let normalized_twice = normalized_once.clone().normalize();
            assert_eq!(
                normalized_once, normalized_twice,
                "Normalization should be idempotent for {:?}",
                val
            );
        }
    }

    #[test]
    fn test_normalize_edge_cases() {
        // Empty sets should remain as-is (though they're invalid)
        let empty_int_set = AbstractValue::IntSet(HashSet::new());
        let normalized = empty_int_set.clone().normalize();
        assert!(matches!(normalized, AbstractValue::IntSet(_)));

        let empty_bool_set = AbstractValue::BoolSet(HashSet::new());
        let normalized = empty_bool_set.clone().normalize();
        assert!(matches!(normalized, AbstractValue::BoolSet(_)));

        let empty_symbol_set = AbstractValue::SymbolSet(HashSet::new());
        let normalized = empty_symbol_set.clone().normalize();
        assert!(matches!(normalized, AbstractValue::SymbolSet(_)));
    }

    #[test]
    fn test_normalize_negative_intervals() {
        // Test normalization with negative numbers
        let val = AbstractValue::int_interval(-5, -5);
        let normalized = val.normalize();
        assert_eq!(normalized, AbstractValue::int_constant(-5));

        let val = AbstractValue::int_interval(-10, 10);
        let normalized = val.normalize();
        assert_eq!(normalized, AbstractValue::int_interval(-10, 10));
    }

    #[test]
    fn test_normalize_zero() {
        // Test normalization with zero
        let val = AbstractValue::int_interval(0, 0);
        let normalized = val.normalize();
        assert_eq!(normalized, AbstractValue::int_constant(0));

        let val = AbstractValue::int_set(vec![0]);
        let normalized = val.normalize();
        assert_eq!(normalized, AbstractValue::int_constant(0));
    }

    #[test]
    fn test_normalize_large_numbers() {
        // Test normalization with large numbers
        let large = i64::MAX;
        let val = AbstractValue::int_interval(large, large);
        let normalized = val.normalize();
        assert_eq!(normalized, AbstractValue::int_constant(large));

        let val = AbstractValue::int_set(vec![large]);
        let normalized = val.normalize();
        assert_eq!(normalized, AbstractValue::int_constant(large));
    }

    // === Arithmetic Operations Tests ===

    #[test]
    fn test_add_concrete_concrete() {
        let a = AbstractValue::int_constant(5);
        let b = AbstractValue::int_constant(10);
        assert_eq!(a.add(&b), AbstractValue::int_constant(15));
    }

    #[test]
    fn test_add_concrete_overflow() {
        let a = AbstractValue::int_constant(i64::MAX);
        let b = AbstractValue::int_constant(1);
        assert_eq!(a.add(&b), AbstractValue::PositiveInfinity);
    }

    #[test]
    fn test_add_interval_interval() {
        let a = AbstractValue::int_interval(0, 5);
        let b = AbstractValue::int_interval(10, 20);
        assert_eq!(a.add(&b), AbstractValue::int_interval(10, 25));
    }

    #[test]
    fn test_add_interval_singleton() {
        let a = AbstractValue::int_interval(5, 5);
        let b = AbstractValue::int_interval(10, 10);
        assert_eq!(a.add(&b), AbstractValue::int_constant(15));
    }

    #[test]
    fn test_add_set_set() {
        let a = AbstractValue::int_set(vec![1, 2, 3]);
        let b = AbstractValue::int_set(vec![10, 20]);
        let result = a.add(&b);
        // Result should be {11, 12, 13, 21, 22, 23}
        if let AbstractValue::IntSet(set) = result {
            assert_eq!(set.len(), 6);
            assert!(set.contains(&11));
            assert!(set.contains(&12));
            assert!(set.contains(&13));
            assert!(set.contains(&21));
            assert!(set.contains(&22));
            assert!(set.contains(&23));
        } else {
            panic!("Expected IntSet");
        }
    }

    #[test]
    fn test_add_constant_set() {
        let a = AbstractValue::int_constant(5);
        let b = AbstractValue::int_set(vec![10, 20]);
        let result = a.add(&b);
        // Result should be {15, 25}
        if let AbstractValue::IntSet(set) = result {
            assert_eq!(set.len(), 2);
            assert!(set.contains(&15));
            assert!(set.contains(&25));
        } else {
            panic!("Expected IntSet");
        }
    }

    #[test]
    fn test_add_positive_infinity() {
        let a = AbstractValue::PositiveInfinity;
        let b = AbstractValue::int_constant(5);
        assert_eq!(a.add(&b), AbstractValue::PositiveInfinity);
    }

    #[test]
    fn test_add_negative_infinity() {
        let a = AbstractValue::NegativeInfinity;
        let b = AbstractValue::int_constant(5);
        assert_eq!(a.add(&b), AbstractValue::NegativeInfinity);
    }

    #[test]
    fn test_sub_concrete_concrete() {
        let a = AbstractValue::int_constant(10);
        let b = AbstractValue::int_constant(5);
        assert_eq!(a.sub(&b), AbstractValue::int_constant(5));
    }

    #[test]
    fn test_sub_interval_interval() {
        let a = AbstractValue::int_interval(10, 20);
        let b = AbstractValue::int_interval(0, 5);
        assert_eq!(a.sub(&b), AbstractValue::int_interval(5, 20));
    }

    #[test]
    fn test_sub_underflow() {
        let a = AbstractValue::int_constant(i64::MIN);
        let b = AbstractValue::int_constant(1);
        assert_eq!(a.sub(&b), AbstractValue::NegativeInfinity);
    }

    #[test]
    fn test_sub_concrete_interval() {
        let a = AbstractValue::int_constant(10);
        let b = AbstractValue::int_interval(2, 5);
        // 10 - [2, 5] = [10-5, 10-2] = [5, 8]
        assert_eq!(a.sub(&b), AbstractValue::int_interval(5, 8));
    }

    #[test]
    fn test_sub_interval_concrete() {
        let a = AbstractValue::int_interval(10, 20);
        let b = AbstractValue::int_constant(5);
        // [10, 20] - 5 = [5, 15]
        assert_eq!(a.sub(&b), AbstractValue::int_interval(5, 15));
    }

    #[test]
    fn test_sub_interval_singleton() {
        let a = AbstractValue::int_interval(10, 20);
        let b = AbstractValue::int_interval(5, 5);
        // [10, 20] - [5, 5] = [5, 15]
        assert_eq!(a.sub(&b), AbstractValue::int_interval(5, 15));
    }

    #[test]
    fn test_sub_set_set() {
        let a = AbstractValue::int_set(vec![10, 20]);
        let b = AbstractValue::int_set(vec![3, 5]);
        // {10, 20} - {3, 5} = {10-3, 10-5, 20-3, 20-5} = {7, 5, 17, 15}
        let result = a.sub(&b);
        assert!(matches!(result, AbstractValue::IntSet(_)));
        if let AbstractValue::IntSet(set) = result {
            assert_eq!(set.len(), 4);
            assert!(set.contains(&7));
            assert!(set.contains(&5));
            assert!(set.contains(&17));
            assert!(set.contains(&15));
        }
    }

    #[test]
    fn test_sub_constant_set() {
        let a = AbstractValue::int_constant(10);
        let b = AbstractValue::int_set(vec![2, 3, 5]);
        // 10 - {2, 3, 5} = {8, 7, 5}
        let result = a.sub(&b);
        assert!(matches!(result, AbstractValue::IntSet(_)));
        if let AbstractValue::IntSet(set) = result {
            assert_eq!(set.len(), 3);
            assert!(set.contains(&8));
            assert!(set.contains(&7));
            assert!(set.contains(&5));
        }
    }

    #[test]
    fn test_sub_set_constant() {
        let a = AbstractValue::int_set(vec![10, 20, 30]);
        let b = AbstractValue::int_constant(5);
        // {10, 20, 30} - 5 = {5, 15, 25}
        let result = a.sub(&b);
        assert!(matches!(result, AbstractValue::IntSet(_)));
        if let AbstractValue::IntSet(set) = result {
            assert_eq!(set.len(), 3);
            assert!(set.contains(&5));
            assert!(set.contains(&15));
            assert!(set.contains(&25));
        }
    }

    #[test]
    fn test_sub_interval_set() {
        let a = AbstractValue::int_interval(10, 20);
        let b = AbstractValue::int_set(vec![2, 5]);
        // [10, 20] - {2, 5} = [10-5, 20-2] = [5, 18] (approximation)
        let result = a.sub(&b);
        assert_eq!(result, AbstractValue::int_interval(5, 18));
    }

    #[test]
    fn test_sub_positive_infinity() {
        let pos_inf = AbstractValue::PositiveInfinity;
        let constant = AbstractValue::int_constant(5);

        // +∞ - 5 = +∞
        assert_eq!(pos_inf.sub(&constant), AbstractValue::PositiveInfinity);

        // 5 - (+∞) = -∞
        assert_eq!(constant.sub(&pos_inf), AbstractValue::NegativeInfinity);

        // +∞ - (+∞) = unknown
        assert_eq!(pos_inf.sub(&pos_inf), AbstractValue::IntTop);
    }

    #[test]
    fn test_sub_negative_infinity() {
        let neg_inf = AbstractValue::NegativeInfinity;
        let constant = AbstractValue::int_constant(5);

        // -∞ - 5 = -∞
        assert_eq!(neg_inf.sub(&constant), AbstractValue::NegativeInfinity);

        // 5 - (-∞) = +∞
        assert_eq!(constant.sub(&neg_inf), AbstractValue::PositiveInfinity);

        // -∞ - (-∞) = unknown
        assert_eq!(neg_inf.sub(&neg_inf), AbstractValue::IntTop);
    }

    #[test]
    fn test_sub_mixed_infinity() {
        let pos_inf = AbstractValue::PositiveInfinity;
        let neg_inf = AbstractValue::NegativeInfinity;

        // +∞ - (-∞) = +∞
        assert_eq!(pos_inf.sub(&neg_inf), AbstractValue::PositiveInfinity);

        // -∞ - (+∞) = -∞
        assert_eq!(neg_inf.sub(&pos_inf), AbstractValue::NegativeInfinity);
    }

    #[test]
    fn test_sub_undefined() {
        let undefined = AbstractValue::Undefined;
        let constant = AbstractValue::int_constant(5);

        // undefined - anything = undefined
        assert_eq!(undefined.sub(&constant), AbstractValue::Undefined);
        assert_eq!(constant.sub(&undefined), AbstractValue::Undefined);
        assert_eq!(undefined.sub(&undefined), AbstractValue::Undefined);
    }

    #[test]
    fn test_sub_top() {
        let int_top = AbstractValue::int_top();
        let constant = AbstractValue::int_constant(5);

        // IntTop - anything = IntTop
        assert_eq!(int_top.sub(&constant), AbstractValue::IntTop);
        assert_eq!(constant.sub(&int_top), AbstractValue::IntTop);
        assert_eq!(int_top.sub(&int_top), AbstractValue::IntTop);
    }

    #[test]
    fn test_sub_normalization() {
        // Test that results are normalized
        let a = AbstractValue::int_interval(10, 10);
        let b = AbstractValue::int_interval(5, 5);
        // [10, 10] - [5, 5] = [5, 5] should normalize to IntConstant(5)
        let result = a.sub(&b);
        assert_eq!(result, AbstractValue::int_constant(5));

        let c = AbstractValue::int_set(vec![10]);
        let d = AbstractValue::int_set(vec![5]);
        // {10} - {5} = {5} should normalize to IntConstant(5)
        let result2 = c.sub(&d);
        assert_eq!(result2, AbstractValue::int_constant(5));
    }

    #[test]
    fn test_mul_concrete_concrete() {
        let a = AbstractValue::int_constant(5);
        let b = AbstractValue::int_constant(10);
        assert_eq!(a.mul(&b), AbstractValue::int_constant(50));
    }

    #[test]
    fn test_mul_zero() {
        let a = AbstractValue::int_constant(5);
        let b = AbstractValue::int_constant(0);
        assert_eq!(a.mul(&b), AbstractValue::int_constant(0));
    }

    #[test]
    fn test_mul_interval_interval_non_negative() {
        let a = AbstractValue::int_interval(0, 5);
        let b = AbstractValue::int_interval(10, 20);
        let result = a.mul(&b);
        // Result should be [0, 100]
        assert_eq!(result, AbstractValue::int_interval(0, 100));
    }

    #[test]
    fn test_mul_interval_interval_negative() {
        let a = AbstractValue::int_interval(-5, -1);
        let b = AbstractValue::int_interval(2, 4);
        let result = a.mul(&b);
        // Result should be [-20, -2] (negative * positive = negative)
        assert_eq!(result, AbstractValue::int_interval(-20, -2));
    }

    #[test]
    fn test_mul_constant_interval() {
        let a = AbstractValue::int_constant(3);
        let b = AbstractValue::int_interval(5, 10);
        let result = a.mul(&b);
        assert_eq!(result, AbstractValue::int_interval(15, 30));
    }

    #[test]
    fn test_mul_constant_interval_negative() {
        let a = AbstractValue::int_constant(-2);
        let b = AbstractValue::int_interval(5, 10);
        let result = a.mul(&b);
        // Negative multiplier reverses bounds
        assert_eq!(result, AbstractValue::int_interval(-20, -10));
    }

    #[test]
    fn test_mul_overflow() {
        let a = AbstractValue::int_constant(i64::MAX);
        let b = AbstractValue::int_constant(2);
        assert_eq!(a.mul(&b), AbstractValue::PositiveInfinity);
    }

    #[test]
    fn test_mul_interval_interval_mixed_signs() {
        // Mixed signs: [-3, 7] * [-2, 3]
        // Corners: (-3)*(-2)=6, (-3)*3=-9, 7*(-2)=-14, 7*3=21
        // Result: [-14, 21] (min of corners is -14, max is 21)
        let a = AbstractValue::int_interval(-3, 7);
        let b = AbstractValue::int_interval(-2, 3);
        let result = a.mul(&b);
        assert_eq!(result, AbstractValue::int_interval(-14, 21));
    }

    #[test]
    fn test_mul_interval_interval_with_zero() {
        // Interval containing zero: [-3, 7] * [1, 1] = [-3, 7]
        let a = AbstractValue::int_interval(-3, 7);
        let b = AbstractValue::int_interval(1, 1);
        let result = a.mul(&b);
        assert_eq!(result, AbstractValue::int_interval(-3, 7));
    }

    #[test]
    fn test_mul_set_set() {
        let a = AbstractValue::int_set(vec![2, 3]);
        let b = AbstractValue::int_set(vec![4, 5]);
        // {2, 3} * {4, 5} = {2*4, 2*5, 3*4, 3*5} = {8, 10, 12, 15}
        let result = a.mul(&b);
        assert!(matches!(result, AbstractValue::IntSet(_)));
        if let AbstractValue::IntSet(set) = result {
            assert_eq!(set.len(), 4);
            assert!(set.contains(&8));
            assert!(set.contains(&10));
            assert!(set.contains(&12));
            assert!(set.contains(&15));
        }
    }

    #[test]
    fn test_mul_constant_set() {
        let a = AbstractValue::int_constant(3);
        let b = AbstractValue::int_set(vec![2, 4, 5]);
        // 3 * {2, 4, 5} = {6, 12, 15}
        let result = a.mul(&b);
        assert!(matches!(result, AbstractValue::IntSet(_)));
        if let AbstractValue::IntSet(set) = result {
            assert_eq!(set.len(), 3);
            assert!(set.contains(&6));
            assert!(set.contains(&12));
            assert!(set.contains(&15));
        }
    }

    #[test]
    fn test_mul_set_constant() {
        let a = AbstractValue::int_set(vec![2, 3, 4]);
        let b = AbstractValue::int_constant(5);
        // {2, 3, 4} * 5 = {10, 15, 20}
        let result = a.mul(&b);
        assert!(matches!(result, AbstractValue::IntSet(_)));
        if let AbstractValue::IntSet(set) = result {
            assert_eq!(set.len(), 3);
            assert!(set.contains(&10));
            assert!(set.contains(&15));
            assert!(set.contains(&20));
        }
    }

    #[test]
    fn test_mul_constant_set_zero() {
        let a = AbstractValue::int_constant(0);
        let b = AbstractValue::int_set(vec![2, 3, 4]);
        // 0 * {2, 3, 4} = 0
        assert_eq!(a.mul(&b), AbstractValue::int_constant(0));
    }

    // ===== Lattice Operations Tests =====

    #[test]
    fn test_join_constant_constant() {
        let a = AbstractValue::int_constant(3);
        let b = AbstractValue::int_constant(5);
        assert_eq!(a.join(&b), AbstractValue::int_interval(3, 5));
    }

    #[test]
    fn test_join_constant_constant_same() {
        let a = AbstractValue::int_constant(5);
        let b = AbstractValue::int_constant(5);
        assert_eq!(a.join(&b), AbstractValue::int_constant(5)); // Normalized
    }

    #[test]
    fn test_join_interval_interval() {
        let a = AbstractValue::int_interval(0, 10);
        let b = AbstractValue::int_interval(5, 15);
        assert_eq!(a.join(&b), AbstractValue::int_interval(0, 15));
    }

    #[test]
    fn test_join_set_set() {
        let a = AbstractValue::int_set(vec![1, 2, 3]);
        let b = AbstractValue::int_set(vec![3, 4, 5]);
        let result = a.join(&b);
        assert!(matches!(result, AbstractValue::IntSet(_)));
        if let AbstractValue::IntSet(set) = result {
            assert_eq!(set.len(), 5);
            assert!(set.contains(&1));
            assert!(set.contains(&2));
            assert!(set.contains(&3));
            assert!(set.contains(&4));
            assert!(set.contains(&5));
        }
    }

    #[test]
    fn test_join_set_set_singleton_result() {
        let a = AbstractValue::int_set(vec![5]);
        let b = AbstractValue::int_set(vec![5]);
        assert_eq!(a.join(&b), AbstractValue::int_constant(5)); // Normalized
    }

    #[test]
    fn test_join_constant_interval() {
        let a = AbstractValue::int_constant(5);
        let b = AbstractValue::int_interval(10, 20);
        assert_eq!(a.join(&b), AbstractValue::int_interval(5, 20));
    }

    #[test]
    fn test_join_bool_constant_constant() {
        let a = AbstractValue::bool_constant(true);
        let b = AbstractValue::bool_constant(false);
        let result = a.join(&b);
        assert!(matches!(result, AbstractValue::BoolSet(_)));
        if let AbstractValue::BoolSet(set) = result {
            assert_eq!(set.len(), 2);
            assert!(set.contains(&true));
            assert!(set.contains(&false));
        }
    }

    #[test]
    fn test_join_symbol_constant_constant() {
        let a = AbstractValue::symbol_constant("a".to_string());
        let b = AbstractValue::symbol_constant("b".to_string());
        let result = a.join(&b);
        assert!(matches!(result, AbstractValue::SymbolSet(_)));
        if let AbstractValue::SymbolSet(set) = result {
            assert_eq!(set.len(), 2);
            assert!(set.contains("a"));
            assert!(set.contains("b"));
        }
    }

    #[test]
    fn test_join_with_top() {
        let a = AbstractValue::int_constant(5);
        let b = AbstractValue::int_top();
        assert_eq!(a.join(&b), AbstractValue::IntTop);
    }

    #[test]
    fn test_meet_interval_interval() {
        let a = AbstractValue::int_interval(0, 10);
        let b = AbstractValue::int_interval(5, 15);
        assert_eq!(a.meet(&b), AbstractValue::int_interval(5, 10));
    }

    #[test]
    fn test_meet_interval_interval_empty() {
        let a = AbstractValue::int_interval(0, 5);
        let b = AbstractValue::int_interval(10, 15);
        assert_eq!(a.meet(&b), AbstractValue::Undefined); // Empty intersection
    }

    #[test]
    fn test_meet_interval_interval_singleton() {
        let a = AbstractValue::int_interval(0, 10);
        let b = AbstractValue::int_interval(5, 5);
        assert_eq!(a.meet(&b), AbstractValue::int_constant(5)); // Normalized
    }

    #[test]
    fn test_meet_constant_constant() {
        let a = AbstractValue::int_constant(5);
        let b = AbstractValue::int_constant(5);
        assert_eq!(a.meet(&b), AbstractValue::int_constant(5));
    }

    #[test]
    fn test_meet_constant_constant_different() {
        let a = AbstractValue::int_constant(3);
        let b = AbstractValue::int_constant(5);
        assert_eq!(a.meet(&b), AbstractValue::Undefined); // Empty
    }

    #[test]
    fn test_meet_set_set() {
        let a = AbstractValue::int_set(vec![1, 2, 3, 4]);
        let b = AbstractValue::int_set(vec![3, 4, 5, 6]);
        let result = a.meet(&b);
        assert!(matches!(result, AbstractValue::IntSet(_)));
        if let AbstractValue::IntSet(set) = result {
            assert_eq!(set.len(), 2);
            assert!(set.contains(&3));
            assert!(set.contains(&4));
        }
    }

    #[test]
    fn test_meet_constant_interval() {
        let a = AbstractValue::int_constant(5);
        let b = AbstractValue::int_interval(0, 10);
        assert_eq!(a.meet(&b), AbstractValue::int_constant(5));
    }

    #[test]
    fn test_meet_constant_interval_outside() {
        let a = AbstractValue::int_constant(15);
        let b = AbstractValue::int_interval(0, 10);
        assert_eq!(a.meet(&b), AbstractValue::Undefined);
    }

    #[test]
    fn test_meet_interval_set() {
        let a = AbstractValue::int_interval(0, 10);
        let b = AbstractValue::int_set(vec![3, 5, 7, 15]);
        let result = a.meet(&b);
        assert!(matches!(result, AbstractValue::IntSet(_)));
        if let AbstractValue::IntSet(set) = result {
            assert_eq!(set.len(), 3);
            assert!(set.contains(&3));
            assert!(set.contains(&5));
            assert!(set.contains(&7));
            assert!(!set.contains(&15)); // Outside interval
        }
    }

    #[test]
    fn test_meet_with_top() {
        let a = AbstractValue::int_interval(5, 10);
        let b = AbstractValue::int_top();
        assert_eq!(a.meet(&b), AbstractValue::int_interval(5, 10)); // Top is identity
    }

    #[test]
    fn test_widen_interval_interval_no_extension() {
        let a = AbstractValue::int_interval(0, 10);
        let b = AbstractValue::int_interval(5, 8); // Contained within a
        let result = a.widen(&b);
        assert_eq!(result, AbstractValue::int_interval(0, 10));
    }

    #[test]
    fn test_widen_interval_interval_extends_upper() {
        let a = AbstractValue::int_interval(0, 10);
        let b = AbstractValue::int_interval(5, 20); // Extends upper bound
        let result = a.widen(&b);
        // Should extend to IntTop since upper bound increased
        assert_eq!(result, AbstractValue::IntTop);
    }

    #[test]
    fn test_widen_interval_interval_extends_lower() {
        let a = AbstractValue::int_interval(10, 20);
        let b = AbstractValue::int_interval(5, 15); // Extends lower bound
        let result = a.widen(&b);
        // Should extend to IntTop since lower bound decreased
        assert_eq!(result, AbstractValue::IntTop);
    }

    #[test]
    fn test_widen_set_set_small() {
        let a = AbstractValue::int_set(vec![1, 2, 3]);
        let b = AbstractValue::int_set(vec![3, 4, 5]);
        let result = a.widen(&b);
        // Union is small, should stay as set
        assert!(matches!(result, AbstractValue::IntSet(_)));
    }

    #[test]
    fn test_widen_bool_same_as_join() {
        let a = AbstractValue::bool_constant(true);
        let b = AbstractValue::bool_constant(false);
        let join_result = a.join(&b);
        let widen_result = a.widen(&b);
        assert_eq!(join_result, widen_result); // Same for finite domains
    }

    #[test]
    fn test_narrow_interval_interval() {
        let a = AbstractValue::int_interval(0, 20);
        let b = AbstractValue::int_interval(5, 15);
        assert_eq!(a.narrow(&b), AbstractValue::int_interval(5, 15));
    }

    #[test]
    fn test_narrow_interval_interval_empty() {
        let a = AbstractValue::int_interval(0, 5);
        let b = AbstractValue::int_interval(10, 15);
        assert_eq!(a.narrow(&b), AbstractValue::Undefined);
    }

    #[test]
    fn test_narrow_set_set() {
        let a = AbstractValue::int_set(vec![1, 2, 3, 4, 5]);
        let b = AbstractValue::int_set(vec![3, 4, 5, 6, 7]);
        let result = a.narrow(&b);
        assert!(matches!(result, AbstractValue::IntSet(_)));
        if let AbstractValue::IntSet(set) = result {
            assert_eq!(set.len(), 3);
            assert!(set.contains(&3));
            assert!(set.contains(&4));
            assert!(set.contains(&5));
        }
    }

    #[test]
    fn test_narrow_set_set_singleton() {
        let a = AbstractValue::int_set(vec![1, 2, 3]);
        let b = AbstractValue::int_set(vec![2, 4]);
        assert_eq!(a.narrow(&b), AbstractValue::int_constant(2)); // Normalized
    }

    #[test]
    fn test_mul_set_constant_zero() {
        let a = AbstractValue::int_set(vec![2, 3, 4]);
        let b = AbstractValue::int_constant(0);
        // {2, 3, 4} * 0 = 0
        assert_eq!(a.mul(&b), AbstractValue::int_constant(0));
    }

    #[test]
    fn test_mul_interval_set() {
        let a = AbstractValue::int_interval(2, 5);
        let b = AbstractValue::int_set(vec![3, 4]);
        // [2, 5] * {3, 4} = [2*3, 5*4] = [6, 20] (approximation)
        let result = a.mul(&b);
        assert_eq!(result, AbstractValue::int_interval(6, 20));
    }

    #[test]
    fn test_mul_interval_negative_constant() {
        // Test negative constant multiplier
        let a = AbstractValue::int_interval(5, 10);
        let b = AbstractValue::int_constant(-2);
        // [5, 10] * (-2) = [-20, -10] (bounds reversed)
        let result = a.mul(&b);
        assert_eq!(result, AbstractValue::int_interval(-20, -10));
    }

    #[test]
    fn test_mul_positive_infinity() {
        let pos_inf = AbstractValue::PositiveInfinity;
        let constant = AbstractValue::int_constant(5);

        // +∞ * 5 = +∞
        assert_eq!(pos_inf.mul(&constant), AbstractValue::PositiveInfinity);

        // 5 * (+∞) = +∞
        assert_eq!(constant.mul(&pos_inf), AbstractValue::PositiveInfinity);

        // +∞ * (-5) = -∞
        let neg_constant = AbstractValue::int_constant(-5);
        assert_eq!(pos_inf.mul(&neg_constant), AbstractValue::NegativeInfinity);

        // +∞ * (+∞) = +∞
        assert_eq!(pos_inf.mul(&pos_inf), AbstractValue::PositiveInfinity);
    }

    #[test]
    fn test_mul_negative_infinity() {
        let neg_inf = AbstractValue::NegativeInfinity;
        let constant = AbstractValue::int_constant(5);

        // -∞ * 5 = -∞
        assert_eq!(neg_inf.mul(&constant), AbstractValue::NegativeInfinity);

        // 5 * (-∞) = -∞
        assert_eq!(constant.mul(&neg_inf), AbstractValue::NegativeInfinity);

        // -∞ * (-5) = +∞
        let neg_constant = AbstractValue::int_constant(-5);
        assert_eq!(neg_inf.mul(&neg_constant), AbstractValue::PositiveInfinity);

        // -∞ * (-∞) = +∞
        assert_eq!(neg_inf.mul(&neg_inf), AbstractValue::PositiveInfinity);
    }

    #[test]
    fn test_mul_mixed_infinity() {
        let pos_inf = AbstractValue::PositiveInfinity;
        let neg_inf = AbstractValue::NegativeInfinity;

        // +∞ * (-∞) = -∞
        assert_eq!(pos_inf.mul(&neg_inf), AbstractValue::NegativeInfinity);

        // -∞ * (+∞) = -∞
        assert_eq!(neg_inf.mul(&pos_inf), AbstractValue::NegativeInfinity);
    }

    #[test]
    fn test_mul_zero_infinity() {
        let zero = AbstractValue::int_constant(0);
        let pos_inf = AbstractValue::PositiveInfinity;
        let neg_inf = AbstractValue::NegativeInfinity;

        // 0 * (+∞) = undefined
        assert_eq!(zero.mul(&pos_inf), AbstractValue::IntTop);
        assert_eq!(pos_inf.mul(&zero), AbstractValue::IntTop);

        // 0 * (-∞) = undefined
        assert_eq!(zero.mul(&neg_inf), AbstractValue::IntTop);
        assert_eq!(neg_inf.mul(&zero), AbstractValue::IntTop);
    }

    #[test]
    fn test_mul_undefined() {
        let undefined = AbstractValue::Undefined;
        let constant = AbstractValue::int_constant(5);

        // undefined * anything = undefined
        assert_eq!(undefined.mul(&constant), AbstractValue::Undefined);
        assert_eq!(constant.mul(&undefined), AbstractValue::Undefined);
        assert_eq!(undefined.mul(&undefined), AbstractValue::Undefined);
    }

    #[test]
    fn test_mul_top() {
        let int_top = AbstractValue::int_top();
        let constant = AbstractValue::int_constant(5);

        // IntTop * anything = IntTop
        assert_eq!(int_top.mul(&constant), AbstractValue::IntTop);
        assert_eq!(constant.mul(&int_top), AbstractValue::IntTop);
        assert_eq!(int_top.mul(&int_top), AbstractValue::IntTop);
    }

    #[test]
    fn test_mul_normalization() {
        // Test that results are normalized
        let a = AbstractValue::int_interval(5, 5);
        let b = AbstractValue::int_interval(2, 2);
        // [5, 5] * [2, 2] = [10, 10] should normalize to IntConstant(10)
        let result = a.mul(&b);
        assert_eq!(result, AbstractValue::int_constant(10));

        let c = AbstractValue::int_set(vec![5]);
        let d = AbstractValue::int_set(vec![2]);
        // {5} * {2} = {10} should normalize to IntConstant(10)
        let result2 = c.mul(&d);
        assert_eq!(result2, AbstractValue::int_constant(10));
    }

    #[test]
    fn test_mul_negative_overflow() {
        // Test negative overflow
        let a = AbstractValue::int_constant(i64::MIN);
        let b = AbstractValue::int_constant(-1);
        // i64::MIN * (-1) would overflow, but we handle it
        let result = a.mul(&b);
        // This should be PositiveInfinity (negative * negative = positive)
        assert_eq!(result, AbstractValue::PositiveInfinity);
    }

    #[test]
    fn test_mul_both_negative_intervals() {
        // Both negative: [-5, -1] * [-4, -2]
        // Corners: (-5)*(-4)=20, (-5)*(-2)=10, (-1)*(-4)=4, (-1)*(-2)=2
        // Result: [2, 20] (negative * negative = positive)
        let a = AbstractValue::int_interval(-5, -1);
        let b = AbstractValue::int_interval(-4, -2);
        let result = a.mul(&b);
        assert_eq!(result, AbstractValue::int_interval(2, 20));
    }

    #[test]
    fn test_mul_positive_negative_interval() {
        // Positive * negative: [2, 3] * [-4, -2]
        // Corners: 2*(-4)=-8, 2*(-2)=-4, 3*(-4)=-12, 3*(-2)=-6
        // Result: [-12, -4]
        let a = AbstractValue::int_interval(2, 3);
        let b = AbstractValue::int_interval(-4, -2);
        let result = a.mul(&b);
        assert_eq!(result, AbstractValue::int_interval(-12, -4));
    }

    #[test]
    fn test_div_concrete_concrete() {
        let a = AbstractValue::int_constant(10);
        let b = AbstractValue::int_constant(2);
        assert_eq!(a.div(&b), Ok(AbstractValue::int_constant(5)));
    }

    #[test]
    fn test_div_by_zero() {
        let a = AbstractValue::int_constant(10);
        let b = AbstractValue::int_constant(0);
        assert_eq!(a.div(&b), Err(()));
    }

    #[test]
    fn test_div_interval_interval() {
        let a = AbstractValue::int_interval(0, 10);
        let b = AbstractValue::int_interval(2, 5);
        let result = a.div(&b);
        // Result should be [0, 5]
        assert_eq!(result, Ok(AbstractValue::int_interval(0, 5)));
    }

    #[test]
    fn test_div_interval_contains_zero() {
        let a = AbstractValue::int_interval(10, 20);
        let b = AbstractValue::int_interval(-2, 2); // Contains zero
        assert_eq!(a.div(&b), Err(()));
    }

    #[test]
    fn test_div_constant_interval() {
        let a = AbstractValue::int_constant(20);
        let b = AbstractValue::int_interval(4, 5);
        let result = a.div(&b);
        // 20 / [4, 5] = [20/5, 20/4] = [4, 5]
        assert_eq!(result, Ok(AbstractValue::int_interval(4, 5)));
    }

    #[test]
    fn test_div_constant_interval_negative() {
        let a = AbstractValue::int_constant(20);
        let b = AbstractValue::int_interval(-5, -4);
        let result = a.div(&b);
        // 20 / [-5, -4] = [20/(-4), 20/(-5)] = [-5, -4] (reversed)
        assert_eq!(result, Ok(AbstractValue::int_interval(-5, -4)));
    }

    #[test]
    fn test_div_set_set() {
        let a = AbstractValue::int_set(vec![10, 20]);
        let b = AbstractValue::int_set(vec![2, 5]);
        let result = a.div(&b);
        // Result should be {5, 2, 10, 4}
        if let Ok(AbstractValue::IntSet(set)) = result {
            assert_eq!(set.len(), 4);
            assert!(set.contains(&5));
            assert!(set.contains(&2));
            assert!(set.contains(&10));
            assert!(set.contains(&4));
        } else {
            panic!("Expected IntSet");
        }
    }

    #[test]
    fn test_div_set_contains_zero() {
        let a = AbstractValue::int_set(vec![10, 20]);
        let b = AbstractValue::int_set(vec![0, 2]);
        assert_eq!(a.div(&b), Err(()));
    }

    #[test]
    fn test_div_interval_concrete() {
        let a = AbstractValue::int_interval(10, 20);
        let b = AbstractValue::int_constant(5);
        // [10, 20] / 5 = [2, 4]
        let result = a.div(&b);
        assert_eq!(result, Ok(AbstractValue::int_interval(2, 4)));
    }

    #[test]
    fn test_div_interval_concrete_negative() {
        let a = AbstractValue::int_interval(10, 20);
        let b = AbstractValue::int_constant(-5);
        // [10, 20] / (-5) = [-4, -2] (reversed bounds)
        let result = a.div(&b);
        assert_eq!(result, Ok(AbstractValue::int_interval(-4, -2)));
    }

    #[test]
    fn test_div_constant_set() {
        let a = AbstractValue::int_constant(20);
        let b = AbstractValue::int_set(vec![2, 4, 5]);
        // 20 / {2, 4, 5} = {10, 5, 4}
        let result = a.div(&b);
        assert!(result.is_ok());
        if let Ok(AbstractValue::IntSet(set)) = result {
            assert_eq!(set.len(), 3);
            assert!(set.contains(&10));
            assert!(set.contains(&5));
            assert!(set.contains(&4));
        } else {
            panic!("Expected IntSet");
        }
    }

    #[test]
    fn test_div_set_constant() {
        let a = AbstractValue::int_set(vec![10, 20, 30]);
        let b = AbstractValue::int_constant(5);
        // {10, 20, 30} / 5 = {2, 4, 6}
        let result = a.div(&b);
        assert!(result.is_ok());
        if let Ok(AbstractValue::IntSet(set)) = result {
            assert_eq!(set.len(), 3);
            assert!(set.contains(&2));
            assert!(set.contains(&4));
            assert!(set.contains(&6));
        } else {
            panic!("Expected IntSet");
        }
    }

    #[test]
    fn test_div_interval_set() {
        let a = AbstractValue::int_interval(10, 20);
        let b = AbstractValue::int_set(vec![2, 5]);
        // [10, 20] / {2, 5} = [10/5, 20/2] = [2, 10] (approximation)
        let result = a.div(&b);
        assert!(result.is_ok());
        if let Ok(AbstractValue::IntInterval(min, max)) = result {
            assert!(min <= 2 && max >= 10); // Approximate bounds
        } else {
            panic!("Expected IntInterval");
        }
    }

    #[test]
    fn test_div_interval_interval_negative() {
        // Both negative: [-10, -5] / [-4, -2]
        // Corners: (-10)/(-4)=2, (-10)/(-2)=5, (-5)/(-4)=1, (-5)/(-2)=2
        // Result: [1, 5]
        let a = AbstractValue::int_interval(-10, -5);
        let b = AbstractValue::int_interval(-4, -2);
        let result = a.div(&b);
        assert_eq!(result, Ok(AbstractValue::int_interval(1, 5)));
    }

    #[test]
    fn test_div_interval_interval_mixed_signs() {
        // Mixed signs: [-10, 10] / [2, 4]
        // Corners: (-10)/2=-5, (-10)/4=-2, 10/2=5, 10/4=2
        // Result: [-5, 5]
        let a = AbstractValue::int_interval(-10, 10);
        let b = AbstractValue::int_interval(2, 4);
        let result = a.div(&b);
        assert_eq!(result, Ok(AbstractValue::int_interval(-5, 5)));
    }

    #[test]
    fn test_div_positive_infinity() {
        let pos_inf = AbstractValue::PositiveInfinity;
        let constant = AbstractValue::int_constant(5);

        // +∞ / 5 = +∞
        assert_eq!(pos_inf.div(&constant), Ok(AbstractValue::PositiveInfinity));

        // 5 / (+∞) = 0
        assert_eq!(constant.div(&pos_inf), Ok(AbstractValue::int_constant(0)));

        // +∞ / (-5) = -∞
        let neg_constant = AbstractValue::int_constant(-5);
        assert_eq!(
            pos_inf.div(&neg_constant),
            Ok(AbstractValue::NegativeInfinity)
        );

        // +∞ / (+∞) = undefined
        assert_eq!(pos_inf.div(&pos_inf), Ok(AbstractValue::IntTop));
    }

    #[test]
    fn test_div_negative_infinity() {
        let neg_inf = AbstractValue::NegativeInfinity;
        let constant = AbstractValue::int_constant(5);

        // -∞ / 5 = -∞
        assert_eq!(neg_inf.div(&constant), Ok(AbstractValue::NegativeInfinity));

        // 5 / (-∞) = 0
        assert_eq!(constant.div(&neg_inf), Ok(AbstractValue::int_constant(0)));

        // -∞ / (-5) = +∞
        let neg_constant = AbstractValue::int_constant(-5);
        assert_eq!(
            neg_inf.div(&neg_constant),
            Ok(AbstractValue::PositiveInfinity)
        );

        // -∞ / (-∞) = undefined
        assert_eq!(neg_inf.div(&neg_inf), Ok(AbstractValue::IntTop));
    }

    #[test]
    fn test_div_mixed_infinity() {
        let pos_inf = AbstractValue::PositiveInfinity;
        let neg_inf = AbstractValue::NegativeInfinity;

        // +∞ / (-∞) = undefined
        assert_eq!(pos_inf.div(&neg_inf), Ok(AbstractValue::IntTop));

        // -∞ / (+∞) = undefined
        assert_eq!(neg_inf.div(&pos_inf), Ok(AbstractValue::IntTop));
    }

    #[test]
    fn test_div_undefined() {
        let undefined = AbstractValue::Undefined;
        let constant = AbstractValue::int_constant(5);

        // undefined / anything = undefined
        assert_eq!(undefined.div(&constant), Ok(AbstractValue::Undefined));
        assert_eq!(constant.div(&undefined), Ok(AbstractValue::Undefined));
        assert_eq!(undefined.div(&undefined), Ok(AbstractValue::Undefined));
    }

    #[test]
    fn test_div_top() {
        let int_top = AbstractValue::int_top();
        let constant = AbstractValue::int_constant(5);

        // IntTop / anything = IntTop
        assert_eq!(int_top.div(&constant), Ok(AbstractValue::IntTop));
        assert_eq!(constant.div(&int_top), Ok(AbstractValue::IntTop));
        assert_eq!(int_top.div(&int_top), Ok(AbstractValue::IntTop));
    }

    #[test]
    fn test_div_normalization() {
        // Test that results are normalized
        let a = AbstractValue::int_interval(10, 10);
        let b = AbstractValue::int_interval(2, 2);
        // [10, 10] / [2, 2] = [5, 5] should normalize to IntConstant(5)
        let result = a.div(&b);
        assert_eq!(result, Ok(AbstractValue::int_constant(5)));

        let c = AbstractValue::int_set(vec![10]);
        let d = AbstractValue::int_set(vec![2]);
        // {10} / {2} = {5} should normalize to IntConstant(5)
        let result2 = c.div(&d);
        assert_eq!(result2, Ok(AbstractValue::int_constant(5)));
    }

    #[test]
    fn test_div_overflow() {
        // Test division overflow: i64::MIN / -1
        let a = AbstractValue::int_constant(i64::MIN);
        let b = AbstractValue::int_constant(-1);
        // i64::MIN / -1 would overflow, should return IntTop
        let result = a.div(&b);
        assert_eq!(result, Ok(AbstractValue::IntTop));
    }

    #[test]
    fn test_div_interval_zero_boundary() {
        // Test interval that touches zero but doesn't contain it
        let a = AbstractValue::int_interval(10, 20);
        let b = AbstractValue::int_interval(1, 5); // Doesn't contain zero
        let result = a.div(&b);
        assert!(result.is_ok());

        // Test interval that contains zero
        let c = AbstractValue::int_interval(10, 20);
        let d = AbstractValue::int_interval(-1, 1); // Contains zero
        assert_eq!(c.div(&d), Err(()));
    }

    #[test]
    fn test_div_negative_numerator_positive_denominator() {
        // Negative numerator, positive denominator
        let a = AbstractValue::int_interval(-20, -10);
        let b = AbstractValue::int_interval(2, 5);
        // [-20, -10] / [2, 5] = [-10, -2]
        let result = a.div(&b);
        assert_eq!(result, Ok(AbstractValue::int_interval(-10, -2)));
    }

    #[test]
    fn test_div_positive_numerator_negative_denominator() {
        // Positive numerator, negative denominator
        let a = AbstractValue::int_interval(10, 20);
        let b = AbstractValue::int_interval(-5, -2);
        // [10, 20] / [-5, -2] = [-10, -2] (reversed)
        let result = a.div(&b);
        assert_eq!(result, Ok(AbstractValue::int_interval(-10, -2)));
    }

    #[test]
    fn test_arithmetic_normalization() {
        // Test that arithmetic operations normalize results
        let a = AbstractValue::int_interval(5, 5);
        let b = AbstractValue::int_constant(0);
        let result = a.add(&b);
        assert_eq!(result, AbstractValue::int_constant(5)); // Normalized

        let a = AbstractValue::int_set(vec![10]);
        let b = AbstractValue::int_constant(0);
        let result = a.add(&b);
        assert_eq!(result, AbstractValue::int_constant(10)); // Normalized
    }

    #[test]
    fn test_arithmetic_type_combinations() {
        // Test various type combinations
        let constant = AbstractValue::int_constant(5);
        let interval = AbstractValue::int_interval(10, 20);
        let set = AbstractValue::int_set(vec![1, 2, 3]);

        // Constant + Interval
        let result = constant.add(&interval);
        assert_eq!(result, AbstractValue::int_interval(15, 25));

        // Interval + Set
        let result = interval.add(&set);
        // Should convert set to interval bounds
        assert!(matches!(
            result,
            AbstractValue::IntInterval(_, _) | AbstractValue::IntTop
        ));

        // Set + Constant
        let result = set.add(&constant);
        if let AbstractValue::IntSet(result_set) = result {
            assert_eq!(result_set.len(), 3);
            assert!(result_set.contains(&6));
            assert!(result_set.contains(&7));
            assert!(result_set.contains(&8));
        } else {
            panic!("Expected IntSet");
        }
    }

    #[test]
    fn test_arithmetic_special_cases() {
        // Test special infinity cases
        let pos_inf = AbstractValue::PositiveInfinity;
        let neg_inf = AbstractValue::NegativeInfinity;

        // +∞ + (-∞) = unknown
        assert_eq!(pos_inf.add(&neg_inf), AbstractValue::IntTop);

        // +∞ - (+∞) = unknown
        assert_eq!(pos_inf.sub(&pos_inf), AbstractValue::IntTop);

        // +∞ * (-∞) = -∞
        assert_eq!(pos_inf.mul(&neg_inf), AbstractValue::NegativeInfinity);

        // 0 * ∞ = undefined
        let zero = AbstractValue::int_constant(0);
        assert_eq!(zero.mul(&pos_inf), AbstractValue::IntTop);
    }
}
