//! Shamir secret sharing and Lagrange interpolation over the BLS12-381 scalar field Fr.
//!
//! All arithmetic is pure safe Rust operating on `[u64; 4]` little-endian limbs.

use anyhow::{bail, Result};
use rand::RngCore;

/// BLS12-381 scalar field modulus r, in little-endian u64 limbs.
const MODULUS: [u64; 4] = [
    0xffff_ffff_0000_0001,
    0x53bd_a402_fffe_5bfe,
    0x3339_d808_09a1_d805,
    0x73ed_a753_299d_7d48,
];

/// 2^256 mod r (precomputed).  Used for reducing 512-bit products.
const R2: [u64; 4] = [
    0x0000_0001_ffff_fffe,
    0x5884_b7fa_0003_4802,
    0x998c_4fef_ecbc_4ff5,
    0x1824_b159_acc5_056f,
];

// --------------------------------------------------------------------------
// 256-bit helpers (safe Rust, no allocations)
// --------------------------------------------------------------------------

fn is_zero(a: &[u64; 4]) -> bool {
    a[0] == 0 && a[1] == 0 && a[2] == 0 && a[3] == 0
}

fn geq(a: &[u64; 4], b: &[u64; 4]) -> bool {
    for i in (0..4).rev() {
        if a[i] > b[i] {
            return true;
        }
        if a[i] < b[i] {
            return false;
        }
    }
    true
}

/// a + b with no reduction.  Caller must ensure result < 2^256.
fn add_raw(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let mut r = [0u64; 4];
    let mut carry = 0u128;
    for i in 0..4 {
        let s = a[i] as u128 + b[i] as u128 + carry;
        r[i] = s as u64;
        carry = s >> 64;
    }
    r // carry is always 0 here when both inputs are < r (since 2r < 2^256)
}

/// a - b.  Assumes a >= b.
fn sub_raw(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let mut r = [0u64; 4];
    let mut borrow = 0i128;
    for i in 0..4 {
        let d = a[i] as i128 - b[i] as i128 - borrow;
        if d < 0 {
            r[i] = (d + (1i128 << 64)) as u64;
            borrow = 1;
        } else {
            r[i] = d as u64;
            borrow = 0;
        }
    }
    r
}

/// (a + b) mod MODULUS.
fn fr_add(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let r = add_raw(a, b);
    if geq(&r, &MODULUS) { sub_raw(&r, &MODULUS) } else { r }
}

/// (a - b) mod MODULUS.
fn fr_sub(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    if geq(a, b) {
        sub_raw(a, b)
    } else {
        sub_raw(&add_raw(&MODULUS, a), b)
    }
}

/// Reduce a 256-bit value (possibly >= MODULUS) into [0, MODULUS).
fn reduce_256(a: &[u64; 4]) -> [u64; 4] {
    let mut r = *a;
    if geq(&r, &MODULUS) { r = sub_raw(&r, &MODULUS); }
    if geq(&r, &MODULUS) { r = sub_raw(&r, &MODULUS); }
    r
}

/// Right-shift a 256-bit limb array by 1 bit in place.
fn shr1(a: &mut [u64; 4]) {
    a[0] = (a[0] >> 1) | (a[1] << 63);
    a[1] = (a[1] >> 1) | (a[2] << 63);
    a[2] = (a[2] >> 1) | (a[3] << 63);
    a[3] >>= 1;
}

/// Halve `a` mod MODULUS (i.e., compute a/2 mod r).
/// If a is even, divide directly; if odd, add MODULUS first (keeping result even).
fn half_mod(a: &[u64; 4]) -> [u64; 4] {
    if a[0] & 1 == 0 {
        let mut r = *a;
        shr1(&mut r);
        r
    } else {
        // (a + MODULUS) is even (both odd), safe since a < MODULUS so a + MODULUS < 2*MODULUS < 2^256
        let sum = add_raw(a, &MODULUS);
        let mut r = sum;
        shr1(&mut r);
        r
    }
}

/// 256 × 256 → 512-bit schoolbook multiplication.
fn mul_512(a: &[u64; 4], b: &[u64; 4]) -> [u64; 8] {
    let mut r = [0u64; 8];
    for i in 0..4 {
        let mut carry = 0u128;
        for j in 0..4 {
            let prod = a[i] as u128 * b[j] as u128 + r[i + j] as u128 + carry;
            r[i + j] = prod as u64;
            carry = prod >> 64;
        }
        r[i + 4] += carry as u64;
    }
    r
}

/// Reduce a 512-bit number t mod MODULUS using double-and-add on the high half.
///
/// Computes: t_lo + t_hi * (2^256 mod r) where t_hi is processed bit-by-bit.
fn reduce_512(t: &[u64; 8]) -> [u64; 4] {
    let t_lo = [t[0], t[1], t[2], t[3]];
    let t_hi = [t[4], t[5], t[6], t[7]];

    let mut result = reduce_256(&t_lo);
    let mut acc = R2; // 2^256 mod r; doubles each iteration to represent 2^(256+i) mod r
    let mut hi = t_hi;

    for _ in 0..256 {
        if hi[0] & 1 == 1 {
            result = fr_add(&result, &acc);
        }
        shr1(&mut hi);
        acc = fr_add(&acc, &acc); // double acc = 2^(256+i+1) mod r
    }
    result
}

/// Full Fr multiplication: (a * b) mod MODULUS.
fn fr_mul(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let t = mul_512(a, b);
    reduce_512(&t)
}

/// Binary extended GCD: compute a^{-1} mod MODULUS.
/// Uses only shifts, additions, and subtractions — no multiplication.
/// Returns None if a == 0.
fn fr_inv_bgcd(a: &[u64; 4]) -> Option<[u64; 4]> {
    if is_zero(a) {
        return None;
    }
    // Invariants: u ≡ s·a (mod MODULUS), v ≡ t·a (mod MODULUS)
    let mut u = *a;
    let mut v = MODULUS;
    let mut s = [1u64, 0, 0, 0]; // s * a ≡ u
    let mut t = [0u64; 4]; // t * a ≡ v

    for _ in 0..512 {
        if is_zero(&u) {
            // gcd = v; should be 1
            return if v == [1, 0, 0, 0] { Some(t) } else { None };
        }
        if u[0] & 1 == 0 {
            shr1(&mut u);
            s = half_mod(&s);
        } else if v[0] & 1 == 0 {
            shr1(&mut v);
            t = half_mod(&t);
        } else if geq(&u, &v) {
            u = fr_sub(&u, &v);
            shr1(&mut u);
            s = fr_sub(&s, &t);
            s = half_mod(&s);
        } else {
            v = fr_sub(&v, &u);
            shr1(&mut v);
            t = fr_sub(&t, &s);
            t = half_mod(&t);
        }
    }
    None // did not converge (should not happen for prime modulus)
}

// --------------------------------------------------------------------------
// Fr field element
// --------------------------------------------------------------------------

/// An element of the BLS12-381 scalar field Fr, stored in little-endian limbs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fr(pub [u64; 4]);

impl Fr {
    pub const ZERO: Fr = Fr([0, 0, 0, 0]);
    pub const ONE: Fr = Fr([1, 0, 0, 0]);

    /// From a small non-negative integer.
    pub fn from_u64(n: u64) -> Self {
        Fr([n, 0, 0, 0])
    }

    /// From a signed integer (handles negation mod r for negative values).
    pub fn from_i64(n: i64) -> Self {
        if n >= 0 {
            Fr([n as u64, 0, 0, 0])
        } else {
            // -n mod r: since n < 0 and |n| ≤ 2^63, MODULUS[0] > |n| so no borrow into limb 1.
            let pos = (-n) as u64;
            Fr(sub_raw(&MODULUS, &[pos, 0, 0, 0]))
        }
    }

    /// From a u128 value, reducing mod r.
    pub fn from_u128(v: u128) -> Self {
        let lo = Fr::from_u64(v as u64);
        let hi_val = (v >> 64) as u64;
        if hi_val == 0 {
            return lo;
        }
        // v = lo + hi_val * 2^64
        // hi_val * 2^64 mod r: use mul_small on the Fr([0,1,0,0]) = 2^64 representation.
        // Since 2^64 < MODULUS, Fr([0,1,0,0]) is a valid Fr element.
        let two64 = Fr([0, 1, 0, 0]);
        let hi_fr = two64.mul_small(hi_val);
        hi_fr + lo
    }

    /// Little-endian bytes (32 bytes).
    pub fn to_le_bytes(self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, limb) in self.0.iter().enumerate() {
            out[i * 8..(i + 1) * 8].copy_from_slice(&limb.to_le_bytes());
        }
        out
    }

    /// Parse little-endian bytes, reducing mod r.
    pub fn from_le_bytes(b: &[u8; 32]) -> Self {
        let mut limbs = [0u64; 4];
        for i in 0..4 {
            limbs[i] = u64::from_le_bytes(b[i * 8..(i + 1) * 8].try_into().unwrap());
        }
        Fr(reduce_256(&limbs))
    }

    /// Multiply by a small u64 via double-and-add (avoids full 256-bit multiplication).
    pub fn mul_small(self, k: u64) -> Self {
        let mut result = Fr::ZERO;
        let mut base = self;
        let mut k = k;
        while k > 0 {
            if k & 1 == 1 {
                result = result + base;
            }
            base = base + base;
            k >>= 1;
        }
        result
    }

    /// Modular inverse via binary extended GCD.
    pub fn inv(self) -> Option<Self> {
        fr_inv_bgcd(&self.0).map(Fr)
    }
}

impl std::ops::Add for Fr {
    type Output = Fr;
    fn add(self, other: Fr) -> Fr { Fr(fr_add(&self.0, &other.0)) }
}

impl std::ops::Sub for Fr {
    type Output = Fr;
    fn sub(self, other: Fr) -> Fr { Fr(fr_sub(&self.0, &other.0)) }
}

impl std::ops::Neg for Fr {
    type Output = Fr;
    fn neg(self) -> Fr {
        if self == Fr::ZERO { Fr::ZERO } else { Fr(sub_raw(&MODULUS, &self.0)) }
    }
}

impl std::ops::Mul for Fr {
    type Output = Fr;
    fn mul(self, other: Fr) -> Fr { Fr(fr_mul(&self.0, &other.0)) }
}

// --------------------------------------------------------------------------
// Shamir secret sharing
// --------------------------------------------------------------------------

/// A Shamir share: (index in 1..=n, value in Fr).
#[derive(Clone, Debug)]
pub struct Share {
    pub index: u8,
    pub value: Fr,
}

/// Generate a (t, n)-Shamir secret sharing of `secret`.
///
/// Returns exactly n shares, with indices 1..=n.
pub fn generate_shares(secret: Fr, t: u8, n: u8, rng: &mut impl RngCore) -> Vec<Share> {
    assert!(t >= 1 && t <= n, "invalid (t, n): t must be in [1, n]");
    assert!(n <= 64, "n > 64 not supported");

    // Polynomial: f(x) = secret + a1*x + … + a_{t-1}*x^{t-1}
    let mut coeffs: Vec<Fr> = Vec::with_capacity(t as usize);
    coeffs.push(secret);
    for _ in 1..t {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        coeffs.push(Fr::from_le_bytes(&bytes));
    }

    (1..=n)
        .map(|i| Share {
            index: i,
            value: eval_poly_horner(&coeffs, Fr::from_u64(i as u64)),
        })
        .collect()
}

/// Horner evaluation of polynomial with given coefficients at point x.
fn eval_poly_horner(coeffs: &[Fr], x: Fr) -> Fr {
    // f(x) = c0 + c1*x + c2*x^2 + ... = c0 + x*(c1 + x*(c2 + ...))
    let mut result = Fr::ZERO;
    for coeff in coeffs.iter().rev() {
        result = result * x + *coeff;
    }
    result
}

// --------------------------------------------------------------------------
// Lagrange interpolation
// --------------------------------------------------------------------------

/// Compute the Lagrange coefficient λ_j for index j given a subset of indices.
///
/// λ_j = ∏_{k ∈ set, k ≠ j} k / (k − j)   (all arithmetic in Fr)
pub fn lagrange_coefficient(j: u8, set: &[u8]) -> Result<Fr> {
    if !set.contains(&j) {
        bail!("index {} not in participating set {:?}", j, set);
    }

    // Accumulate numerator and denominator as u128 (safe for n ≤ 64, t ≤ 64).
    let mut num: u128 = 1;
    let mut den_abs: u128 = 1;
    let mut den_neg = false;

    for &k in set {
        if k == j {
            continue;
        }
        num = num
            .checked_mul(k as u128)
            .ok_or_else(|| anyhow::anyhow!("Lagrange numerator overflow (n too large)"))?;

        let diff = k as i64 - j as i64;
        if diff > 0 {
            den_abs = den_abs
                .checked_mul(diff as u128)
                .ok_or_else(|| anyhow::anyhow!("Lagrange denominator overflow"))?;
        } else {
            den_abs = den_abs
                .checked_mul((-diff) as u128)
                .ok_or_else(|| anyhow::anyhow!("Lagrange denominator overflow"))?;
            den_neg = !den_neg;
        }
    }

    let num_fr = Fr::from_u128(num);
    let den_fr = Fr::from_u128(den_abs);
    let den_inv = den_fr.inv().ok_or_else(|| anyhow::anyhow!("zero Lagrange denominator"))?;

    let lambda = num_fr * den_inv;
    Ok(if den_neg { -lambda } else { lambda })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fr_add_sub_roundtrip() {
        let a = Fr::from_u64(42);
        let b = Fr::from_u64(100);
        assert_eq!(a + b - b, a);
    }

    #[test]
    fn test_fr_neg() {
        let a = Fr::from_u64(7);
        let neg_a = -a;
        assert_eq!(a + neg_a, Fr::ZERO);
    }

    #[test]
    fn test_fr_inv() {
        let a = Fr::from_u64(3);
        let inv = a.inv().expect("3 has inverse mod r");
        assert_eq!(a * inv, Fr::ONE);
    }

    #[test]
    fn test_fr_mul() {
        let a = Fr::from_u64(6);
        let b = Fr::from_u64(7);
        assert_eq!(a * b, Fr::from_u64(42));
    }

    #[test]
    fn test_fr_from_u128() {
        // 2^64 as u128 = a specific Fr element
        let v: u128 = 1u128 << 64;
        let fr = Fr::from_u128(v);
        // Should equal Fr([0, 1, 0, 0]) which is 2^64 as Fr (since 2^64 < MODULUS)
        assert_eq!(fr, Fr([0, 1, 0, 0]));
    }

    #[test]
    fn test_lagrange_roundtrip() {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let secret = Fr::from_u64(12345678);
        let shares = generate_shares(secret, 3, 5, &mut rng);

        // Reconstruct from shares {1, 2, 3}
        let subset = [shares[0].clone(), shares[1].clone(), shares[2].clone()];
        let indices: Vec<u8> = subset.iter().map(|s| s.index).collect();

        let mut reconstructed = Fr::ZERO;
        for share in &subset {
            let lambda = lagrange_coefficient(share.index, &indices).unwrap();
            reconstructed = reconstructed + lambda * share.value;
        }
        assert_eq!(reconstructed, secret);
    }

    #[test]
    fn test_lagrange_different_subsets() {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(99);
        let secret = Fr::from_u64(99999);
        let shares = generate_shares(secret, 3, 5, &mut rng);

        // Reconstruct from shares {2, 4, 5}
        let subset = [shares[1].clone(), shares[3].clone(), shares[4].clone()];
        let indices: Vec<u8> = subset.iter().map(|s| s.index).collect();

        let mut reconstructed = Fr::ZERO;
        for share in &subset {
            let lambda = lagrange_coefficient(share.index, &indices).unwrap();
            reconstructed = reconstructed + lambda * share.value;
        }
        assert_eq!(reconstructed, secret);
    }
}
