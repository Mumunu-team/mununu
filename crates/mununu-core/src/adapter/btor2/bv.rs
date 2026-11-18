//! Arbitrary-width concrete bit-vector value for the BTOR2 evaluator.
//!
//! [`Bv`] retires the `u128` ceiling of the old `BvValue`: it is a two-variant
//! value that stays on native `u128` for widths ≤ 128 (the `Small` variant —
//! the overwhelmingly common case, and byte-for-byte the previous masked-`u128`
//! semantics) and promotes to an arbitrary-precision [`num_bigint::BigUint`]
//! magnitude for wider values (the `Wide` variant). Every value is kept
//! **canonically masked** to its `width`, so equality is structural.
//!
//! ## Semantics
//!
//! All arithmetic is fixed-width modular: a same-width op wraps at `2^width`,
//! width-growing ops (`concat`, `uext`, `sext`) produce the wider width, and
//! `slice` narrows. Signed ops (`sra`, `slt`, `sext`) interpret the top bit of
//! the operand width as the sign. This mirrors the BTOR2 operator set the
//! concrete evaluator (`bit_blast`) applies.
//!
//! ## Why a two-variant enum, not a uniform limb type
//!
//! The `Small` path reuses the exact `u128` arithmetic the previous `BvValue`
//! used, so for every width ≤ 128 the result is **behaviour-identical by
//! construction** — the migration cannot shift a verdict on the corpus (all of
//! whose values are ≤ 128 bits). Only the genuinely-wide path is new, and it
//! defers to an established bignum rather than hand-rolled limb arithmetic. The
//! `bv_matches_u128_*` differential tests below pin the two paths together.

use num_bigint::BigUint;
use num_traits::Zero;

/// A concrete, canonically-masked bit-vector value of a fixed `width`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bv {
    /// `width ≤ 128`; `bits` is masked to `width` low bits.
    Small { bits: u128, width: u32 },
    /// `width > 128`; `mag` is masked to `width` low bits.
    Wide { mag: BigUint, width: u32 },
}

/// Mask a `u128` to its low `width` bits (`width ≤ 128`).
#[inline]
fn mask_u128(bits: u128, width: u32) -> u128 {
    if width >= 128 {
        bits
    } else if width == 0 {
        0
    } else {
        bits & ((1u128 << width) - 1)
    }
}

/// The all-ones magnitude of `width` bits, as a `BigUint`.
fn biguint_mask(width: u32) -> BigUint {
    (BigUint::from(1u8) << width) - BigUint::from(1u8)
}

impl Bv {
    /// Build from a `u128` at `width ≤ 128`. Panics if `width > 128` (callers
    /// with wider values use [`Bv::from_biguint`]).
    #[inline]
    pub fn from_u128(bits: u128, width: u32) -> Self {
        assert!(width <= 128, "Bv::from_u128 width {width} > 128");
        Bv::Small {
            bits: mask_u128(bits, width),
            width,
        }
    }

    /// Build from a `BigUint` magnitude at any `width`, masking to `width`
    /// bits and collapsing to the `Small` variant when `width ≤ 128`.
    pub fn from_biguint(mag: BigUint, width: u32) -> Self {
        if width <= 128 {
            let masked = mask_u128(biguint_to_u128_truncating(&mag), width);
            Bv::Small {
                bits: masked,
                width,
            }
        } else {
            Bv::Wide {
                mag: mag & biguint_mask(width),
                width,
            }
        }
    }

    /// Build from a `u128` seed at any `width`: masks to `width` when
    /// `width ≤ 128`, else zero-extends the `u128` into the wider field (the
    /// seed value carries ≤ 128 bits of information). Used at the concrete
    /// evaluator's `u128` seed boundary (register / input values) where a cell
    /// may be wider than 128 bits.
    pub fn from_u128_width(bits: u128, width: u32) -> Self {
        if width <= 128 {
            Bv::from_u128(bits, width)
        } else {
            // `bits < 2^128 ≤ 2^width`, so no masking is needed.
            Bv::Wide {
                mag: BigUint::from(bits),
                width,
            }
        }
    }

    pub fn zero(width: u32) -> Self {
        if width <= 128 {
            Bv::Small { bits: 0, width }
        } else {
            Bv::Wide {
                mag: BigUint::zero(),
                width,
            }
        }
    }

    pub fn one(width: u32) -> Self {
        Bv::from_biguint(BigUint::from(1u8), width)
    }

    pub fn ones(width: u32) -> Self {
        if width <= 128 {
            Bv::Small {
                bits: mask_u128(u128::MAX, width),
                width,
            }
        } else {
            Bv::Wide {
                mag: biguint_mask(width),
                width,
            }
        }
    }

    pub fn from_bool(b: bool) -> Self {
        Bv::Small {
            bits: b as u128,
            width: 1,
        }
    }

    #[inline]
    pub fn width(&self) -> u32 {
        match self {
            Bv::Small { width, .. } | Bv::Wide { width, .. } => *width,
        }
    }

    #[inline]
    pub fn is_zero(&self) -> bool {
        match self {
            Bv::Small { bits, .. } => *bits == 0,
            Bv::Wide { mag, .. } => mag.is_zero(),
        }
    }

    #[inline]
    pub fn is_nonzero(&self) -> bool {
        !self.is_zero()
    }

    #[inline]
    pub fn to_bool(&self) -> bool {
        self.is_nonzero()
    }

    /// The value as a `u128` when it fits (width ≤ 128), else `None`.
    #[inline]
    pub fn to_u128(&self) -> Option<u128> {
        match self {
            Bv::Small { bits, .. } => Some(*bits),
            Bv::Wide { .. } => None,
        }
    }

    /// The low 128 bits of the value (exact for `Small`, truncating for `Wide`).
    /// Used to extract a shift amount or a narrow field from a possibly-wide value.
    #[inline]
    pub fn low128(&self) -> u128 {
        match self {
            Bv::Small { bits, .. } => *bits,
            Bv::Wide { mag, .. } => biguint_to_u128_truncating(mag),
        }
    }

    /// The value as a `BigUint` magnitude (always exact).
    pub fn to_biguint(&self) -> BigUint {
        match self {
            Bv::Small { bits, .. } => BigUint::from(*bits),
            Bv::Wide { mag, .. } => mag.clone(),
        }
    }

    /// `true` iff bit `i` is set (`i` may be ≥ 128; out-of-width bits are 0).
    #[inline]
    pub fn bit(&self, i: u64) -> bool {
        if i as u32 >= self.width() {
            return false;
        }
        match self {
            Bv::Small { bits, .. } => (bits >> i) & 1 == 1,
            Bv::Wide { mag, .. } => mag.bit(i),
        }
    }

    /// `true` iff the sign bit (bit `width-1`) is set.
    pub fn is_negative(&self) -> bool {
        let w = self.width();
        if w == 0 {
            return false;
        }
        match self {
            Bv::Small { bits, .. } => (bits >> (w - 1)) & 1 == 1,
            Bv::Wide { mag, .. } => mag.bit((w - 1) as u64),
        }
    }

    // ---- bitwise (same width) ----

    fn binop_small_or_wide(
        &self,
        other: &Bv,
        small: impl Fn(u128, u128) -> u128,
        wide: impl Fn(BigUint, BigUint) -> BigUint,
    ) -> Bv {
        debug_assert_eq!(self.width(), other.width(), "width mismatch in binop");
        let w = self.width();
        match (self, other) {
            (Bv::Small { bits: a, .. }, Bv::Small { bits: b, .. }) => Bv::Small {
                bits: mask_u128(small(*a, *b), w),
                width: w,
            },
            _ => Bv::from_biguint(wide(self.to_biguint(), other.to_biguint()), w),
        }
    }

    pub fn and(&self, other: &Bv) -> Bv {
        self.binop_small_or_wide(other, |a, b| a & b, |a, b| a & b)
    }
    pub fn or(&self, other: &Bv) -> Bv {
        self.binop_small_or_wide(other, |a, b| a | b, |a, b| a | b)
    }
    pub fn xor(&self, other: &Bv) -> Bv {
        self.binop_small_or_wide(other, |a, b| a ^ b, |a, b| a ^ b)
    }

    pub fn not(&self) -> Bv {
        let w = self.width();
        match self {
            Bv::Small { bits, .. } => Bv::Small {
                bits: mask_u128(!*bits, w),
                width: w,
            },
            Bv::Wide { mag, .. } => Bv::Wide {
                mag: (mag ^ biguint_mask(w)),
                width: w,
            },
        }
    }

    // ---- arithmetic (same width, modular) ----

    pub fn add(&self, other: &Bv) -> Bv {
        self.binop_small_or_wide(other, |a, b| a.wrapping_add(b), |a, b| a + b)
    }

    pub fn sub(&self, other: &Bv) -> Bv {
        // Modular: a - b ≡ a + (2^w - b). Small uses wrapping u128 then mask.
        debug_assert_eq!(self.width(), other.width());
        let w = self.width();
        match (self, other) {
            (Bv::Small { bits: a, .. }, Bv::Small { bits: b, .. }) => Bv::Small {
                bits: mask_u128(a.wrapping_sub(*b), w),
                width: w,
            },
            _ => {
                let modulus = BigUint::from(1u8) << w;
                let a = self.to_biguint();
                let b = other.to_biguint();
                Bv::from_biguint((a + &modulus - b) % modulus, w)
            }
        }
    }

    pub fn mul(&self, other: &Bv) -> Bv {
        debug_assert_eq!(self.width(), other.width());
        let w = self.width();
        match (self, other) {
            // A same-width `u128 * u128` can overflow 128 bits even when the
            // masked result fits `w ≤ 128`; go wide whenever a plain wrapping
            // product could lose bits below the width mask (w > 64), so the
            // masked low `w` bits are always exact.
            (Bv::Small { bits: a, .. }, Bv::Small { bits: b, .. }) if w <= 64 => Bv::Small {
                bits: mask_u128(a.wrapping_mul(*b), w),
                width: w,
            },
            _ => Bv::from_biguint(self.to_biguint() * other.to_biguint(), w),
        }
    }

    pub fn udiv(&self, other: &Bv) -> Bv {
        // BTOR2: division by zero yields all-ones (the SMT-LIB `bvudiv` total
        // extension). Match that.
        let w = self.width();
        if other.is_zero() {
            return Bv::ones(w);
        }
        self.binop_small_or_wide(other, |a, b| a / b, |a, b| a / b)
    }

    pub fn urem(&self, other: &Bv) -> Bv {
        // BTOR2: remainder by zero yields the dividend.
        let w = self.width();
        if other.is_zero() {
            return self.clone();
        }
        let _ = w;
        self.binop_small_or_wide(other, |a, b| a % b, |a, b| a % b)
    }

    /// `|self|` in two's complement (`INT_MIN` stays `INT_MIN`, as the circuit's `neg` wraps).
    fn abs(&self) -> Bv {
        if self.is_negative() {
            self.neg()
        } else {
            self.clone()
        }
    }

    /// SMT-LIB `bvsdiv` (mununu#1055): `|a| udiv |b|`, negated when the signs differ. A zero
    /// divisor follows from `udiv`'s all-ones: all-ones for a non-negative dividend, `1` for a
    /// negative one — the same values the circuit's `Aig::sdiv` produces, so a replay agrees with
    /// the search.
    pub fn sdiv(&self, other: &Bv) -> Bv {
        let q = self.abs().udiv(&other.abs());
        if self.is_negative() != other.is_negative() {
            q.neg()
        } else {
            q
        }
    }

    /// SMT-LIB `bvsrem`: the sign follows the dividend (a zero divisor gives the dividend).
    pub fn srem(&self, other: &Bv) -> Bv {
        let r = self.abs().urem(&other.abs());
        if self.is_negative() { r.neg() } else { r }
    }

    /// SMT-LIB `bvsmod`: the sign follows the divisor (a zero divisor gives the dividend; a zero
    /// remainder stays zero).
    pub fn smod(&self, other: &Bv) -> Bv {
        let u = self.abs().urem(&other.abs());
        if u.is_zero() {
            return u;
        }
        match (self.is_negative(), other.is_negative()) {
            (false, false) => u,
            (true, false) => u.neg().add(other),
            (false, true) => u.add(other),
            (true, true) => u.neg(),
        }
    }

    /// BTOR2 `uaddo`: the unsigned sum does not fit the width.
    pub fn uaddo(&self, other: &Bv) -> bool {
        self.add(other).ult(self).to_bool()
    }

    /// BTOR2 `usubo`: the unsigned difference would be negative.
    pub fn usubo(&self, other: &Bv) -> bool {
        self.ult(other).to_bool()
    }

    /// BTOR2 `saddo`: the operands agree in sign and the sum's sign differs.
    pub fn saddo(&self, other: &Bv) -> bool {
        let r = self.add(other);
        self.is_negative() == other.is_negative() && r.is_negative() != self.is_negative()
    }

    /// BTOR2 `ssubo`: the operands differ in sign and the difference's sign differs from the
    /// minuend's.
    pub fn ssubo(&self, other: &Bv) -> bool {
        let r = self.sub(other);
        self.is_negative() != other.is_negative() && r.is_negative() != self.is_negative()
    }

    /// BTOR2 `umulo`: the unsigned product does not fit the width (its upper half is non-zero).
    pub fn umulo(&self, other: &Bv) -> bool {
        let n = self.width();
        if n == 0 {
            return false;
        }
        let p = self.uext(n).mul(&other.uext(n));
        p.slice(2 * n - 1, n).is_nonzero()
    }

    /// BTOR2 `smulo`: the signed product does not fit the width (bits `n-1 ..= 2n-1` of the
    /// double-width product are not all equal).
    pub fn smulo(&self, other: &Bv) -> bool {
        let n = self.width();
        if n == 0 {
            return false;
        }
        let p = self.sext(n).mul(&other.sext(n));
        let top = p.slice(2 * n - 1, n - 1);
        !(top.is_zero() || top == Bv::ones(n + 1))
    }

    /// BTOR2 `sdivo`: the one overflowing signed division, `INT_MIN / −1`.
    pub fn sdivo(&self, other: &Bv) -> bool {
        let w = self.width();
        if w == 0 {
            return false;
        }
        *self == Bv::one(w).shl(w - 1) && *other == Bv::ones(w)
    }

    pub fn neg(&self) -> Bv {
        Bv::zero(self.width()).sub(self)
    }

    // ---- shifts (result width = self width) ----

    /// Logical left shift by `n` bit positions.
    pub fn shl(&self, n: u32) -> Bv {
        let w = self.width();
        if n >= w {
            return Bv::zero(w);
        }
        match self {
            Bv::Small { bits, .. } => Bv::Small {
                bits: mask_u128(bits << n, w),
                width: w,
            },
            Bv::Wide { mag, .. } => Bv::from_biguint(mag << n, w),
        }
    }

    /// Logical right shift by `n` bit positions.
    pub fn shr(&self, n: u32) -> Bv {
        let w = self.width();
        if n >= w {
            return Bv::zero(w);
        }
        match self {
            Bv::Small { bits, .. } => Bv::Small {
                bits: bits >> n,
                width: w,
            },
            Bv::Wide { mag, .. } => Bv::from_biguint(mag >> n, w),
        }
    }

    /// Arithmetic right shift by `n` (sign-replicating).
    pub fn sra(&self, n: u32) -> Bv {
        let w = self.width();
        let neg = self.is_negative();
        if n >= w {
            return if neg { Bv::ones(w) } else { Bv::zero(w) };
        }
        let logical = self.shr(n);
        if !neg {
            return logical;
        }
        // Fill the top `n` bits with ones: OR with (ones(w) << (w-n)).
        let fill = Bv::ones(w).shl(w - n);
        logical.or(&fill)
    }

    /// Rotate left by `n` bit positions (`n` taken modulo width by the caller).
    pub fn rol(&self, n: u32) -> Bv {
        let w = self.width();
        let n = n % w.max(1);
        if n == 0 {
            return self.clone();
        }
        self.shl(n).or(&self.shr(w - n))
    }

    /// Rotate right by `n` bit positions (`n` taken modulo width by the caller).
    pub fn ror(&self, n: u32) -> Bv {
        let w = self.width();
        let n = n % w.max(1);
        if n == 0 {
            return self.clone();
        }
        self.shr(n).or(&self.shl(w - n))
    }

    /// Population count (number of set bits within the width).
    pub fn count_ones(&self) -> u64 {
        match self {
            Bv::Small { bits, .. } => bits.count_ones() as u64,
            Bv::Wide { mag, .. } => mag.count_ones(),
        }
    }

    // ---- structural ----

    /// Concatenate: `self` becomes the high bits, `low` the low bits. Result
    /// width = `self.width() + low.width()`.
    pub fn concat(&self, low: &Bv) -> Bv {
        let lw = low.width();
        let w = self.width() + lw;
        match (self, low) {
            (Bv::Small { bits: hi, .. }, Bv::Small { bits: lo, .. }) if w <= 128 => Bv::Small {
                bits: mask_u128((hi << lw) | lo, w),
                width: w,
            },
            _ => Bv::from_biguint((self.to_biguint() << lw) | low.to_biguint(), w),
        }
    }

    /// Extract bits `[hi..=lo]` (inclusive, `hi >= lo`, `hi < width`). Result
    /// width = `hi - lo + 1`.
    pub fn slice(&self, hi: u32, lo: u32) -> Bv {
        debug_assert!(hi >= lo && hi < self.width());
        let out_w = hi - lo + 1;
        match self {
            Bv::Small { bits, .. } => Bv::Small {
                bits: mask_u128(bits >> lo, out_w),
                width: out_w,
            },
            Bv::Wide { mag, .. } => Bv::from_biguint(mag >> lo, out_w),
        }
    }

    /// Zero-extend by `by` bits (width grows, value unchanged).
    pub fn uext(&self, by: u32) -> Bv {
        let w = self.width() + by;
        match self {
            Bv::Small { bits, .. } if w <= 128 => Bv::Small {
                bits: *bits,
                width: w,
            },
            _ => Bv::from_biguint(self.to_biguint(), w),
        }
    }

    /// Sign-extend by `by` bits.
    pub fn sext(&self, by: u32) -> Bv {
        let w = self.width() + by;
        if !self.is_negative() || by == 0 {
            return self.uext(by);
        }
        // value | (fill of `by` ones above the original width)
        let base = self.uext(by);
        let fill = Bv::ones(w).shl(self.width());
        base.or(&fill)
    }

    // ---- comparisons (result width 1) ----

    pub fn eq_bv(&self, other: &Bv) -> Bv {
        Bv::from_bool(self == other)
    }
    pub fn ne_bv(&self, other: &Bv) -> Bv {
        Bv::from_bool(self != other)
    }
    pub fn ult(&self, other: &Bv) -> Bv {
        Bv::from_bool(self.ult_bool(other))
    }
    pub fn ulte(&self, other: &Bv) -> Bv {
        Bv::from_bool(self.ult_bool(other) || self == other)
    }
    pub fn ugt(&self, other: &Bv) -> Bv {
        Bv::from_bool(other.ult_bool(self))
    }
    pub fn ugte(&self, other: &Bv) -> Bv {
        Bv::from_bool(other.ult_bool(self) || self == other)
    }
    pub fn slt(&self, other: &Bv) -> Bv {
        Bv::from_bool(self.slt_bool(other))
    }
    pub fn slte(&self, other: &Bv) -> Bv {
        Bv::from_bool(self.slt_bool(other) || self == other)
    }
    pub fn sgt(&self, other: &Bv) -> Bv {
        Bv::from_bool(other.slt_bool(self))
    }
    pub fn sgte(&self, other: &Bv) -> Bv {
        Bv::from_bool(other.slt_bool(self) || self == other)
    }

    fn ult_bool(&self, other: &Bv) -> bool {
        match (self, other) {
            (Bv::Small { bits: a, .. }, Bv::Small { bits: b, .. }) => a < b,
            _ => self.to_biguint() < other.to_biguint(),
        }
    }

    fn slt_bool(&self, other: &Bv) -> bool {
        match (self.is_negative(), other.is_negative()) {
            (true, false) => true,
            (false, true) => false,
            // Same sign → unsigned magnitude order agrees with signed order.
            _ => self.ult_bool(other),
        }
    }
}

/// Truncate a `BigUint` to its low 128 bits (used only when the caller has
/// already established `width ≤ 128`, so the discarded high bits are outside
/// the mask).
fn biguint_to_u128_truncating(mag: &BigUint) -> u128 {
    let masked = mag & ((BigUint::from(1u8) << 128u32) - BigUint::from(1u8));
    let digits = masked.to_u64_digits();
    let lo = digits.first().copied().unwrap_or(0) as u128;
    let hi = digits.get(1).copied().unwrap_or(0) as u128;
    (hi << 64) | lo
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference: a masked u128, the trusted pre-Bv semantics.
    fn mref(bits: u128, width: u32) -> u128 {
        mask_u128(bits, width)
    }

    // Deterministic pseudo-random stream (no Math.random / rand needed): a
    // simple xorshift seeded per-test so widths/values are varied but fixed.
    struct Rng(u128);
    impl Rng {
        fn next(&mut self) -> u128 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn width(&mut self) -> u32 {
            1 + (self.next() % 128) as u32 // 1..=128
        }
    }

    #[test]
    fn bv_small_matches_u128_bitwise_arith_shifts() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15_1234_5678_9ABC_DEF0);
        for _ in 0..20_000 {
            let w = rng.width();
            let a = mref(rng.next(), w);
            let b = mref(rng.next(), w);
            let av = Bv::from_u128(a, w);
            let bv = Bv::from_u128(b, w);

            assert_eq!(av.and(&bv).to_u128(), Some(mref(a & b, w)), "and w={w}");
            assert_eq!(av.or(&bv).to_u128(), Some(mref(a | b, w)), "or w={w}");
            assert_eq!(av.xor(&bv).to_u128(), Some(mref(a ^ b, w)), "xor w={w}");
            assert_eq!(av.not().to_u128(), Some(mref(!a, w)), "not w={w}");
            assert_eq!(
                av.add(&bv).to_u128(),
                Some(mref(a.wrapping_add(b), w)),
                "add w={w} a={a} b={b}"
            );
            assert_eq!(
                av.sub(&bv).to_u128(),
                Some(mref(a.wrapping_sub(b), w)),
                "sub w={w}"
            );
            // mul reference only valid where the wrapping u128 product's low w
            // bits are exact — true for w ≤ 64 (Small fast path) and, for the
            // Bv wide-promoted path, always. Compare against a BigUint oracle.
            let mul_ref = biguint_to_u128_truncating(
                &((BigUint::from(a) * BigUint::from(b)) & biguint_mask(w.min(128))),
            );
            assert_eq!(av.mul(&bv).to_u128(), Some(mref(mul_ref, w)), "mul w={w}");
            if let (Some(q), Some(r)) = (a.checked_div(b), a.checked_rem(b)) {
                assert_eq!(av.udiv(&bv).to_u128(), Some(mref(q, w)), "udiv w={w}");
                assert_eq!(av.urem(&bv).to_u128(), Some(mref(r, w)), "urem w={w}");
            }
            // Shifts by a random amount in 0..=w+2 (covers the ≥ w saturation).
            let n = (rng.next() % (w as u128 + 3)) as u32;
            let shl_ref = if n >= w || n >= 128 {
                0
            } else {
                mref(a << n, w)
            };
            assert_eq!(av.shl(n).to_u128(), Some(shl_ref), "shl w={w} n={n}");
            let shr_ref = if n >= w { 0 } else { a >> n };
            assert_eq!(av.shr(n).to_u128(), Some(shr_ref), "shr w={w} n={n}");
        }
    }

    #[test]
    fn bv_udiv_urem_by_zero_btor2_totalization() {
        // bvudiv by 0 → all-ones; bvurem by 0 → dividend.
        for w in [1u32, 8, 64, 100, 200] {
            let a = Bv::from_biguint(BigUint::from(123u32), w);
            let z = Bv::zero(w);
            assert_eq!(a.udiv(&z), Bv::ones(w), "udiv0 w={w}");
            assert_eq!(a.urem(&z), a.clone(), "urem0 w={w}");
        }
    }

    #[test]
    fn bv_compare_matches_u128() {
        let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D_0F0F_0F0F_0F0F_0F0F);
        for _ in 0..20_000 {
            let w = rng.width();
            let a = mref(rng.next(), w);
            let b = mref(rng.next(), w);
            let av = Bv::from_u128(a, w);
            let bv = Bv::from_u128(b, w);
            assert_eq!(av.ult(&bv).to_bool(), a < b, "ult w={w}");
            assert_eq!(av.ulte(&bv).to_bool(), a <= b, "ulte w={w}");
            assert_eq!(av.eq_bv(&bv).to_bool(), a == b, "eq w={w}");
            // signed: interpret via sign-extension to i128.
            let sa = sign_ext_i128(a, w);
            let sb = sign_ext_i128(b, w);
            assert_eq!(av.slt(&bv).to_bool(), sa < sb, "slt w={w} a={a} b={b}");
            assert_eq!(av.slte(&bv).to_bool(), sa <= sb, "slte w={w}");
        }
    }

    fn sign_ext_i128(bits: u128, width: u32) -> i128 {
        if width == 0 || width >= 128 {
            return bits as i128;
        }
        let sign = (bits >> (width - 1)) & 1 == 1;
        if sign {
            (bits | !((1u128 << width) - 1)) as i128
        } else {
            bits as i128
        }
    }

    #[test]
    fn bv_concat_slice_ext_matches_u128() {
        let mut rng = Rng(0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210);
        for _ in 0..20_000 {
            let wa = 1 + (rng.next() % 64) as u32; // ≤64 so concat ≤128
            let wb = 1 + (rng.next() % 64) as u32;
            let a = mref(rng.next(), wa);
            let b = mref(rng.next(), wb);
            let av = Bv::from_u128(a, wa);
            let bv = Bv::from_u128(b, wb);
            let cat = av.concat(&bv);
            assert_eq!(cat.width(), wa + wb);
            assert_eq!(cat.to_u128(), Some(mref((a << wb) | b, wa + wb)), "concat");
            // slice a random window of `av`.
            let hi = (rng.next() % wa as u128) as u32;
            let lo = (rng.next() % (hi as u128 + 1)) as u32;
            let sl = av.slice(hi, lo);
            assert_eq!(sl.width(), hi - lo + 1);
            assert_eq!(sl.to_u128(), Some(mref(a >> lo, hi - lo + 1)), "slice");
            // uext / sext by a small amount keeping width ≤ 128.
            let by = (rng.next() % (128 - wa) as u128) as u32;
            assert_eq!(av.uext(by).to_u128(), Some(a), "uext");
            let se = sign_ext_i128(a, wa) as u128;
            assert_eq!(av.sext(by).to_u128(), Some(mref(se, wa + by)), "sext");
        }
    }

    #[test]
    fn bv_wide_roundtrips_and_masks() {
        // A >128-bit value: build 200-bit all-ones, check width + masking.
        let w = 200u32;
        let ones = Bv::ones(w);
        assert_eq!(ones.width(), w);
        assert!(matches!(ones, Bv::Wide { .. }));
        // ones + 1 wraps to 0 (mod 2^200).
        assert!(ones.add(&Bv::one(w)).is_zero(), "wrap at 2^200");
        // slice the low 128 collapses to Small and equals all-ones-128.
        let lo = ones.slice(127, 0);
        assert_eq!(lo, Bv::ones(128));
        // concat two 100-bit halves → 200-bit; slice back.
        let h = Bv::from_biguint(BigUint::from(0xABCDu32), 100);
        let l = Bv::from_biguint(BigUint::from(0x1234u32), 100);
        let cat = h.concat(&l);
        assert_eq!(cat.width(), 200);
        assert_eq!(cat.slice(199, 100), h);
        assert_eq!(cat.slice(99, 0), l);
    }

    /// mununu#1055 — the signed-division family and the overflow detectors against integer
    /// arithmetic, every 4-bit pair (zero divisors included), with SMT-LIB's total extensions:
    /// `udiv` by zero is all-ones, the remainders by zero are the dividend, `sdiv` by zero is
    /// all-ones (non-negative dividend) or 1 (negative), `smod`'s sign follows the divisor.
    #[test]
    fn the_division_family_and_overflow_detectors_match_integer_semantics_at_four_bits() {
        let w = 4u32;
        let signed = |v: u128| -> i128 { if v >= 8 { v as i128 - 16 } else { v as i128 } };
        let wrap = |v: i128| -> u128 { (v.rem_euclid(16)) as u128 };
        for a in 0..16u128 {
            for b in 0..16u128 {
                let (x, y) = (Bv::from_u128(a, w), Bv::from_u128(b, w));
                let (sa, sb) = (signed(a), signed(b));
                let udiv = a.checked_div(b).unwrap_or(15);
                let urem = a.checked_rem(b).unwrap_or(a);
                let sdiv = if b == 0 {
                    if sa < 0 { 1 } else { 15 }
                } else {
                    wrap(sa.wrapping_div(sb))
                };
                let srem = if b == 0 { a } else { wrap(sa.wrapping_rem(sb)) };
                let smod = if b == 0 {
                    a
                } else {
                    wrap(((sa % sb) + sb) % sb)
                };
                let ctx = format!("a={a} b={b}");
                assert_eq!(x.udiv(&y).low128(), udiv, "udiv {ctx}");
                assert_eq!(x.urem(&y).low128(), urem, "urem {ctx}");
                assert_eq!(x.sdiv(&y).low128(), sdiv, "sdiv {ctx}");
                assert_eq!(x.srem(&y).low128(), srem, "srem {ctx}");
                assert_eq!(x.smod(&y).low128(), smod, "smod {ctx}");
                assert_eq!(x.uaddo(&y), a + b >= 16, "uaddo {ctx}");
                assert_eq!(x.usubo(&y), a < b, "usubo {ctx}");
                assert_eq!(x.saddo(&y), !(-8..=7).contains(&(sa + sb)), "saddo {ctx}");
                assert_eq!(x.ssubo(&y), !(-8..=7).contains(&(sa - sb)), "ssubo {ctx}");
                assert_eq!(x.umulo(&y), a * b >= 16, "umulo {ctx}");
                assert_eq!(x.smulo(&y), !(-8..=7).contains(&(sa * sb)), "smulo {ctx}");
                assert_eq!(x.sdivo(&y), a == 8 && b == 15, "sdivo {ctx}");
            }
        }
    }
}
