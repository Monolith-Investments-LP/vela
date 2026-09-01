//! BLS12-381 G1 point operations.
//!
//! This module wraps the blst C FFI. All unsafe code is confined here;
//! every unsafe block is annotated with a SAFETY comment. The public
//! functions exposed to the rest of the crate are safe Rust.

use anyhow::{bail, Result};

/// Compressed BLS12-381 G1 affine point (48 bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct G1(pub [u8; 48]);

impl G1 {
    /// The point at infinity (canonical blst-compressed infinity encoding).
    pub fn infinity() -> Self {
        let mut b = [0u8; 48];
        b[0] = 0xc0;
        G1(b)
    }
}

/// Scalar-multiply `point` by `scalar` (little-endian 32 bytes).
pub fn g1_mul(point: &G1, scalar_le: &[u8; 32]) -> Result<G1> {
    // SAFETY: blst functions are memory-safe when given correctly-sized buffers.
    // We validate the decompressed point before multiplying.
    unsafe {
        use blst::{
            blst_p1, blst_p1_affine, blst_p1_affine_compress, blst_p1_from_affine, blst_p1_mult,
            blst_p1_to_affine, blst_p1_uncompress, BLST_ERROR,
        };

        let mut p_aff: blst_p1_affine = std::mem::zeroed();
        let err = blst_p1_uncompress(&mut p_aff, point.0.as_ptr());
        if err != BLST_ERROR::BLST_SUCCESS {
            bail!("g1_mul: invalid G1 point (uncompress failed)");
        }

        let mut p: blst_p1 = std::mem::zeroed();
        blst_p1_from_affine(&mut p, &p_aff);

        let mut result: blst_p1 = std::mem::zeroed();
        blst_p1_mult(&mut result, &p, scalar_le.as_ptr(), 255);

        let mut result_aff: blst_p1_affine = std::mem::zeroed();
        blst_p1_to_affine(&mut result_aff, &result);

        let mut compressed = [0u8; 48];
        blst_p1_affine_compress(compressed.as_mut_ptr(), &result_aff);
        Ok(G1(compressed))
    }
}

/// Add two G1 points.
pub fn g1_add(a: &G1, b: &G1) -> Result<G1> {
    // SAFETY: same as g1_mul — correctly-sized buffers, validated inputs.
    unsafe {
        use blst::{
            blst_p1, blst_p1_add_or_double, blst_p1_affine, blst_p1_affine_compress,
            blst_p1_from_affine, blst_p1_to_affine, blst_p1_uncompress, BLST_ERROR,
        };

        let mut a_aff: blst_p1_affine = std::mem::zeroed();
        let err = blst_p1_uncompress(&mut a_aff, a.0.as_ptr());
        if err != BLST_ERROR::BLST_SUCCESS {
            bail!("g1_add: invalid G1 point a");
        }

        let mut b_aff: blst_p1_affine = std::mem::zeroed();
        let err = blst_p1_uncompress(&mut b_aff, b.0.as_ptr());
        if err != BLST_ERROR::BLST_SUCCESS {
            bail!("g1_add: invalid G1 point b");
        }

        let mut a_proj: blst_p1 = std::mem::zeroed();
        blst_p1_from_affine(&mut a_proj, &a_aff);

        let mut b_proj: blst_p1 = std::mem::zeroed();
        blst_p1_from_affine(&mut b_proj, &b_aff);

        let mut result: blst_p1 = std::mem::zeroed();
        blst_p1_add_or_double(&mut result, &a_proj, &b_proj);

        let mut result_aff: blst_p1_affine = std::mem::zeroed();
        blst_p1_to_affine(&mut result_aff, &result);

        let mut compressed = [0u8; 48];
        blst_p1_affine_compress(compressed.as_mut_ptr(), &result_aff);
        Ok(G1(compressed))
    }
}

/// Negate a G1 point (returns -point).
pub fn g1_neg(point: &G1) -> Result<G1> {
    // SAFETY: same as above.
    unsafe {
        use blst::{
            blst_p1, blst_p1_affine, blst_p1_affine_compress, blst_p1_cneg, blst_p1_from_affine,
            blst_p1_to_affine, blst_p1_uncompress, BLST_ERROR,
        };

        let mut p_aff: blst_p1_affine = std::mem::zeroed();
        let err = blst_p1_uncompress(&mut p_aff, point.0.as_ptr());
        if err != BLST_ERROR::BLST_SUCCESS {
            bail!("g1_neg: invalid G1 point");
        }

        let mut p_proj: blst_p1 = std::mem::zeroed();
        blst_p1_from_affine(&mut p_proj, &p_aff);

        blst_p1_cneg(&mut p_proj, true);

        let mut result_aff: blst_p1_affine = std::mem::zeroed();
        blst_p1_to_affine(&mut result_aff, &p_proj);

        let mut compressed = [0u8; 48];
        blst_p1_affine_compress(compressed.as_mut_ptr(), &result_aff);
        Ok(G1(compressed))
    }
}

/// Multiply the BLS12-381 G1 generator by `scalar_le` (little-endian 32 bytes).
pub fn g1_generator_mul(scalar_le: &[u8; 32]) -> Result<G1> {
    // SAFETY: BLS12_381_G1 is a valid static affine generator defined by blst.
    unsafe {
        use blst::{
            blst_p1, blst_p1_affine, blst_p1_affine_compress, blst_p1_from_affine, blst_p1_mult,
            blst_p1_to_affine, BLS12_381_G1,
        };

        let mut gen_proj: blst_p1 = std::mem::zeroed();
        blst_p1_from_affine(&mut gen_proj, &BLS12_381_G1);

        let mut result: blst_p1 = std::mem::zeroed();
        blst_p1_mult(&mut result, &gen_proj, scalar_le.as_ptr(), 255);

        let mut result_aff: blst_p1_affine = std::mem::zeroed();
        blst_p1_to_affine(&mut result_aff, &result);

        let mut compressed = [0u8; 48];
        blst_p1_affine_compress(compressed.as_mut_ptr(), &result_aff);
        Ok(G1(compressed))
    }
}

/// Validate that `point` is a properly encoded, on-curve, in-subgroup G1 point.
pub fn g1_is_valid(point: &G1) -> bool {
    // SAFETY: blst_p1_affine_in_g1 checks subgroup membership.
    unsafe {
        use blst::{blst_p1_affine, blst_p1_affine_in_g1, blst_p1_uncompress, BLST_ERROR};

        let mut p_aff: blst_p1_affine = std::mem::zeroed();
        if blst_p1_uncompress(&mut p_aff, point.0.as_ptr()) != BLST_ERROR::BLST_SUCCESS {
            return false;
        }
        blst_p1_affine_in_g1(&p_aff)
    }
}

/// Return the compressed G1 generator point bytes.
pub fn g1_generator() -> G1 {
    // SAFETY: BLS12_381_G1 is the well-known BLS12-381 G1 generator constant.
    unsafe {
        use blst::{blst_p1_affine_compress, BLS12_381_G1};
        let mut compressed = [0u8; 48];
        blst_p1_affine_compress(compressed.as_mut_ptr(), &BLS12_381_G1);
        G1(compressed)
    }
}
