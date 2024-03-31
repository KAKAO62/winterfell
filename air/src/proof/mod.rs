// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Contains STARK proof struct and associated components.

use crate::{ProofOptions, TraceInfo, TraceLayout};
use alloc::vec::Vec;
use core::cmp;
use crypto::Hasher;
use fri::FriProof;
use math::FieldElement;
use utils::{ByteReader, Deserializable, DeserializationError, Serializable, SliceReader};

mod context;
pub use context::Context;

mod commitments;
pub use commitments::Commitments;

mod queries;
pub use queries::Queries;

mod ood_frame;
pub use ood_frame::OodFrame;

mod table;
pub use table::Table;

#[cfg(test)]
mod tests;

// CONSTANTS
// ================================================================================================

const GRINDING_CONTRIBUTION_FLOOR: u32 = 80;
const MAX_PROXIMITY_PARAMETER: u64 = 1000;

// STARK PROOF
// ================================================================================================
/// A proof generated by Winterfell prover.
///
/// A STARK proof contains information proving that a computation was executed correctly. A proof
/// also contains basic metadata for the computation, but neither the definition of the computation
/// itself, nor public inputs consumed by the computation are contained in a proof.
///
/// A proof can be serialized into a sequence of bytes using [to_bytes()](StarkProof::to_bytes)
/// function, and deserialized from a sequence of bytes using [from_bytes()](StarkProof::from_bytes)
/// function.
///
/// To estimate soundness of a proof (in bits), [security_level()](StarkProof::security_level)
/// function can be used.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StarkProof {
    /// Basic metadata about the execution of the computation described by this proof.
    pub context: Context,
    /// Number of unique queries made by the verifier. This will be different from the
    /// context.options.num_queries if the same position in the domain was queried more than once.
    pub num_unique_queries: u8,
    /// Commitments made by the prover during the commit phase of the protocol.
    pub commitments: Commitments,
    /// Decommitments of extended execution trace values (for all trace segments) at position
    ///  queried by the verifier.
    pub trace_queries: Vec<Queries>,
    /// Decommitments of constraint composition polynomial evaluations at positions queried by
    /// the verifier.
    pub constraint_queries: Queries,
    /// Trace and constraint polynomial evaluations at an out-of-domain point.
    pub ood_frame: OodFrame,
    /// Low-degree proof for a DEEP composition polynomial.
    pub fri_proof: FriProof,
    /// Proof-of-work nonce for query seed grinding.
    pub pow_nonce: u64,
}

impl StarkProof {
    /// Returns STARK protocol parameters used to generate this proof.
    pub fn options(&self) -> &ProofOptions {
        self.context.options()
    }

    /// Returns a layout describing how columns of the execution trace described by this context
    /// are arranged into segments.
    pub fn trace_layout(&self) -> &TraceLayout {
        self.context.trace_layout()
    }

    /// Returns trace length for the computation described by this proof.
    pub fn trace_length(&self) -> usize {
        self.context.trace_length()
    }

    /// Returns trace info for the computation described by this proof.
    pub fn get_trace_info(&self) -> TraceInfo {
        self.context.get_trace_info()
    }

    /// Returns the size of the LDE domain for the computation described by this proof.
    pub fn lde_domain_size(&self) -> usize {
        self.context.lde_domain_size()
    }

    // SECURITY LEVEL
    // --------------------------------------------------------------------------------------------
    /// Returns security level of this proof (in bits).
    ///
    /// When `conjectured` is true, conjectured security level is returned; otherwise, provable
    /// security level is returned. Usually, the number of queries needed for provable security is
    /// 2x - 3x higher than the number of queries needed for conjectured security at the same
    /// security level.
    pub fn security_level<H: Hasher>(&self, conjectured: bool) -> u32 {
        if conjectured {
            get_conjectured_security(
                self.context.options(),
                self.context.num_modulus_bits(),
                self.trace_length(),
                H::COLLISION_RESISTANCE,
            )
        } else {
            get_proven_security(
                self.context.options(),
                self.context.num_modulus_bits(),
                self.trace_length(),
                H::COLLISION_RESISTANCE,
            )
        }
    }

    // SERIALIZATION / DESERIALIZATION
    // --------------------------------------------------------------------------------------------

    /// Serializes this proof into a vector of bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        Serializable::to_bytes(self)
    }

    /// Returns a STARK proof read from the specified `source`.
    ///
    /// # Errors
    /// Returns an error of a valid STARK proof could not be read from the specified `source`.
    pub fn from_bytes(source: &[u8]) -> Result<Self, DeserializationError> {
        Deserializable::read_from_bytes(source)
    }

    /// Creates a dummy `StarkProof` for use in tests.
    pub fn new_dummy() -> Self {
        use crate::FieldExtension;
        use crypto::hashers::Blake3_192 as DummyHasher;
        use crypto::BatchMerkleProof;
        use math::fields::f64::BaseElement as DummyField;

        Self {
            context: Context::new::<DummyField>(
                &TraceInfo::new(1, 8),
                ProofOptions::new(1, 2, 2, FieldExtension::None, 8, 1),
            ),
            num_unique_queries: 0,
            commitments: Commitments::default(),
            trace_queries: Vec::new(),
            constraint_queries: Queries::new::<_, DummyField>(
                BatchMerkleProof::<DummyHasher<DummyField>> {
                    leaves: Vec::new(),
                    nodes: Vec::new(),
                    depth: 0,
                },
                vec![vec![DummyField::ONE]],
            ),
            ood_frame: OodFrame::default(),
            fri_proof: FriProof::new_dummy(),
            pow_nonce: 0,
        }
    }
}

// SERIALIZATION
// ================================================================================================

impl Serializable for StarkProof {
    fn write_into<W: utils::ByteWriter>(&self, target: &mut W) {
        self.context.write_into(target);
        target.write_u8(self.num_unique_queries);
        self.commitments.write_into(target);
        target.write_many(&self.trace_queries);
        self.constraint_queries.write_into(target);
        self.ood_frame.write_into(target);
        self.fri_proof.write_into(target);
        self.pow_nonce.write_into(target)
    }
}

impl Deserializable for StarkProof {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let context = Context::read_from(source)?;
        let num_unique_queries = source.read_u8()?;
        let commitments = Commitments::read_from(source)?;
        let num_trace_segments = context.trace_layout().num_segments();
        let mut trace_queries = Vec::with_capacity(num_trace_segments);
        for _ in 0..num_trace_segments {
            trace_queries.push(Queries::read_from(source)?);
        }

        let proof = StarkProof {
            context,
            num_unique_queries,
            commitments,
            trace_queries,
            constraint_queries: Queries::read_from(source)?,
            ood_frame: OodFrame::read_from(source)?,
            fri_proof: FriProof::read_from(source)?,
            pow_nonce: source.read_u64()?,
        };
        Ok(proof)
    }
}

// HELPER FUNCTIONS
// ================================================================================================

/// Computes conjectured security level for the specified proof parameters.
fn get_conjectured_security(
    options: &ProofOptions,
    base_field_bits: u32,
    trace_domain_size: usize,
    collision_resistance: u32,
) -> u32 {
    // compute max security we can get for a given field size
    let field_size = base_field_bits * options.field_extension().degree();
    let field_security = field_size - (trace_domain_size * options.blowup_factor()).ilog2();

    // compute security we get by executing multiple query rounds
    let security_per_query = options.blowup_factor().ilog2();
    let mut query_security = security_per_query * options.num_queries() as u32;

    // include grinding factor contributions only for proofs adequate security
    if query_security >= GRINDING_CONTRIBUTION_FLOOR {
        query_security += options.grinding_factor();
    }

    cmp::min(cmp::min(field_security, query_security) - 1, collision_resistance)
}

/// Estimates proven security level for the specified proof parameters.
fn get_proven_security(
    options: &ProofOptions,
    base_field_bits: u32,
    trace_domain_size: usize,
    collision_resistance: u32,
) -> u32 {
    let m_min: usize = 3;
    let m_max = compute_upper_m(trace_domain_size);

    let m_optimal = (m_min as u32..m_max as u32)
        .max_by_key(|&a| {
            proven_security_protocol_for_m(
                options,
                base_field_bits,
                trace_domain_size,
                a as usize,
            )
        })
        .expect(
            "Should not fail since m_max is larger than m_min for all trace sizes of length greater than 4",
        );

    cmp::min(
        proven_security_protocol_for_m(
            options,
            base_field_bits,
            trace_domain_size,
            m_optimal as usize,
        ),
        collision_resistance as u64,
    ) as u32
}

/// Computes proven security level for the specified proof parameters for a fixed
/// value of the proximity parameter m in the list-decoding regime.
fn proven_security_protocol_for_m(
    options: &ProofOptions,
    base_field_bits: u32,
    trace_domain_size: usize,
    m: usize,
) -> u64 {
    let extension_field_bits = (base_field_bits * options.field_extension().degree()) as f64;
    let num_fri_queries = options.num_queries() as f64;
    let m = m as f64;
    let rho = 1.0 / options.blowup_factor() as f64;
    let alpha = (1.0 + 0.5 / m) * sqrt(rho);
    let max_deg = options.blowup_factor() as f64 + 1.0;

    // To apply Theorem 8 in https://eprint.iacr.org/2022/1216.pdf, we need to apply FRI with
    // a slightly larger agreement parameter alpha.
    // More concretely, we need alpha > rho_plus.sqrt() where rho_plus is the rate in function field
    // F(Z) and defined as (trace_domain_size + 2.0) / lde_domain_size .
    // This means that the range of m needs to be restricted in order to ensure that
    // alpha := 1 - theta := rho.sqrt() * (1 + 1/2m) is greater than rho_plus.sqrt().
    // Determining the range of m is the responsibility of the calling function.
    // Now, once m is fixed, we need to make sure that we choose an m_plus such that
    // alpha <= rho_plus.sqrt() * (1 + 1/2m_plus). This m_plus will be used to define
    // the list-decoding list size in F(Z).

    // Modified rate in function field F(Z)
    let lde_domain_size = (trace_domain_size * options.blowup_factor()) as f64;
    let trace_domain_size = trace_domain_size as f64;
    let num_openings = 2.0;
    let rho_plus = (trace_domain_size + num_openings) / lde_domain_size;

    // New proximity parameter m_plus, corresponding to rho_plus, needed to make sure that
    //  alpha < rho_plus.sqrt() * (1 + 1 / (2 * m_plus))
    let m_plus = ceil(1.0 / (2.0 * (alpha / sqrt(rho_plus) - 1.0)));
    let alpha_plus = (1.0 + 0.5 / m_plus) * sqrt(rho_plus);
    let theta_plus = 1.0 - alpha_plus;

    // Computes FRI commit-phase (i.e., pre-query) soundness error.
    // This considers only the first term given in eq. 7 in https://eprint.iacr.org/2022/1216.pdf,
    // i.e. 0.5 * (m + 0.5)^7 * n^2 / (rho^1.5.q) as all other terms are negligible in comparison.
    let fri_commit_err_bits = extension_field_bits
        - log2((0.5 * powf(m + 0.5, 7.0) / powf(rho, 1.5)) * powf(lde_domain_size, 2.0));

    // Compute FRI query-phase soundness error
    let fri_queries_err_bits =
        options.grinding_factor() as f64 - log2(powf(1.0 - theta_plus, num_fri_queries));

    // Combined error for FRI
    let fri_err_bits = cmp::min(fri_commit_err_bits as u64, fri_queries_err_bits as u64);
    if fri_err_bits < 1 {
        return 0;
    }
    let fri_err_bits = fri_err_bits - 1;

    // List size
    let l_plus = (2.0 * m_plus + 1.0) / (2.0 * sqrt(rho_plus));

    // ALI related soundness error. Note that C here is equal to 1 because of our use of
    // linear batching.
    let ali_err_bits = -log2(l_plus) + extension_field_bits;

    // DEEP related soundness error. Note that this uses that the denominator |F| - |D ∪ H|
    // can be approximated by |F| for all practical domain sizes. We also use the blow-up factor
    // as an upper bound for the maximal constraint degree.
    let deep_err_bits = -log2(
        l_plus * (max_deg * (trace_domain_size + num_openings - 1.0) + (trace_domain_size - 1.0)),
    ) + extension_field_bits;

    let min = cmp::min(cmp::min(fri_err_bits, ali_err_bits as u64), deep_err_bits as u64);
    if min < 1 {
        return 0;
    }

    min - 1
}

// HELPER FUNCTIONS
// ================================================================================================

/// Computes the largest proximity parameter m needed for Theorem 8
/// in <https://eprint.iacr.org/2022/1216.pdf> to work.
fn compute_upper_m(h: usize) -> f64 {
    let h = h as f64;
    let m_max = ceil(0.25 * h * (1.0 + sqrt(1.0 + 2.0 / h)));

    // We cap the range to 1000 as the optimal m value will be in the lower range of [m_min, m_max]
    // since increasing m too much will lead to a deterioration in the FRI commit soundness making
    // any benefit gained in the FRI query soundess mute.
    cmp::min(m_max as u64, MAX_PROXIMITY_PARAMETER) as f64
}

#[cfg(feature = "std")]
pub fn log2(value: f64) -> f64 {
    value.log2()
}

#[cfg(not(feature = "std"))]
pub fn log2(value: f64) -> f64 {
    libm::log2(value)
}

#[cfg(feature = "std")]
pub fn sqrt(value: f64) -> f64 {
    value.sqrt()
}

#[cfg(not(feature = "std"))]
pub fn sqrt(value: f64) -> f64 {
    libm::sqrt(value)
}

#[cfg(feature = "std")]
pub fn powf(value: f64, exp: f64) -> f64 {
    value.powf(exp)
}

#[cfg(not(feature = "std"))]
pub fn powf(value: f64, exp: f64) -> f64 {
    libm::pow(value, exp)
}

#[cfg(feature = "std")]
pub fn ceil(value: f64) -> f64 {
    value.ceil()
}

#[cfg(not(feature = "std"))]
pub fn ceil(value: f64) -> f64 {
    libm::ceil(value)
}

#[cfg(test)]
mod prove_security_tests {
    use super::ProofOptions;
    use crate::{proof::get_proven_security, FieldExtension};
    use math::{fields::f64::BaseElement, StarkField};

    #[test]
    fn get_96_bits_security() {
        let field_extension = FieldExtension::Cubic;
        let base_field_bits = BaseElement::MODULUS_BITS;
        let fri_folding_factor = 8;
        let fri_remainder_max_degree = 127;
        let grinding_factor = 20;
        let blowup_factor = 4;
        let num_queries = 80;
        let collision_resistance = 128;
        let trace_length = 2_usize.pow(18);

        let mut options = ProofOptions::new(
            num_queries,
            blowup_factor,
            grinding_factor,
            field_extension,
            fri_folding_factor as usize,
            fri_remainder_max_degree as usize,
        );
        let security_1 =
            get_proven_security(&options, base_field_bits, trace_length, collision_resistance);

        assert_eq!(security_1, 97);

        // increasing the blowup factor should increase the bits of security gained per query
        let blowup_factor = 8;
        let num_queries = 53;

        options = ProofOptions::new(
            num_queries,
            blowup_factor,
            grinding_factor,
            field_extension,
            fri_folding_factor as usize,
            fri_remainder_max_degree as usize,
        );
        let security_2 =
            get_proven_security(&options, base_field_bits, trace_length, collision_resistance);

        assert_eq!(security_2, 97);
    }

    #[test]
    fn get_128_bits_security() {
        let field_extension = FieldExtension::Cubic;
        let base_field_bits = BaseElement::MODULUS_BITS;
        let fri_folding_factor = 8;
        let fri_remainder_max_degree = 127;
        let grinding_factor = 20;
        let blowup_factor = 8;
        let num_queries = 85;
        let collision_resistance = 128;
        let trace_length = 2_usize.pow(18);

        let mut options = ProofOptions::new(
            num_queries,
            blowup_factor,
            grinding_factor,
            field_extension,
            fri_folding_factor as usize,
            fri_remainder_max_degree as usize,
        );
        let security_1 =
            get_proven_security(&options, base_field_bits, trace_length, collision_resistance);

        assert_eq!(security_1, 128);

        // increasing the blowup factor should increase the bits of security gained per query
        let blowup_factor = 16;
        let num_queries = 65;

        options = ProofOptions::new(
            num_queries,
            blowup_factor,
            grinding_factor,
            field_extension,
            fri_folding_factor as usize,
            fri_remainder_max_degree as usize,
        );
        let security_2 =
            get_proven_security(&options, base_field_bits, trace_length, collision_resistance);

        assert_eq!(security_2, 128);
    }

    #[test]
    fn extension_degree() {
        let field_extension = FieldExtension::Quadratic;
        let base_field_bits = BaseElement::MODULUS_BITS;
        let fri_folding_factor = 8;
        let fri_remainder_max_degree = 127;
        let grinding_factor = 20;
        let blowup_factor = 8;
        let num_queries = 85;
        let collision_resistance = 128;
        let trace_length = 2_usize.pow(18);

        let mut options = ProofOptions::new(
            num_queries,
            blowup_factor,
            grinding_factor,
            field_extension,
            fri_folding_factor as usize,
            fri_remainder_max_degree as usize,
        );
        let security_1 =
            get_proven_security(&options, base_field_bits, trace_length, collision_resistance);

        assert_eq!(security_1, 67);

        // increasing the extension degree improves the FRI commit phase soundness error and permits
        // reaching 128 bits security
        let field_extension = FieldExtension::Cubic;

        options = ProofOptions::new(
            num_queries,
            blowup_factor,
            grinding_factor,
            field_extension,
            fri_folding_factor as usize,
            fri_remainder_max_degree as usize,
        );
        let security_2 =
            get_proven_security(&options, base_field_bits, trace_length, collision_resistance);

        assert_eq!(security_2, 128);
    }

    #[test]
    fn trace_length() {
        let field_extension = FieldExtension::Cubic;
        let base_field_bits = BaseElement::MODULUS_BITS;
        let fri_folding_factor = 8;
        let fri_remainder_max_degree = 127;
        let grinding_factor = 20;
        let blowup_factor = 8;
        let num_queries = 80;
        let collision_resistance = 128;
        let trace_length = 2_usize.pow(20);

        let mut options = ProofOptions::new(
            num_queries,
            blowup_factor,
            grinding_factor,
            field_extension,
            fri_folding_factor as usize,
            fri_remainder_max_degree as usize,
        );
        let security_1 =
            get_proven_security(&options, base_field_bits, trace_length, collision_resistance);

        let trace_length = 2_usize.pow(16);

        options = ProofOptions::new(
            num_queries,
            blowup_factor,
            grinding_factor,
            field_extension,
            fri_folding_factor as usize,
            fri_remainder_max_degree as usize,
        );
        let security_2 =
            get_proven_security(&options, base_field_bits, trace_length, collision_resistance);

        assert!(security_1 < security_2);
    }

    #[test]
    fn num_fri_queries() {
        let field_extension = FieldExtension::Cubic;
        let base_field_bits = BaseElement::MODULUS_BITS;
        let fri_folding_factor = 8;
        let fri_remainder_max_degree = 127;
        let grinding_factor = 20;
        let blowup_factor = 8;
        let num_queries = 60;
        let collision_resistance = 128;
        let trace_length = 2_usize.pow(20);

        let mut options = ProofOptions::new(
            num_queries,
            blowup_factor,
            grinding_factor,
            field_extension,
            fri_folding_factor as usize,
            fri_remainder_max_degree as usize,
        );
        let security_1 =
            get_proven_security(&options, base_field_bits, trace_length, collision_resistance);

        let num_queries = 80;

        options = ProofOptions::new(
            num_queries,
            blowup_factor,
            grinding_factor,
            field_extension,
            fri_folding_factor as usize,
            fri_remainder_max_degree as usize,
        );
        let security_2 =
            get_proven_security(&options, base_field_bits, trace_length, collision_resistance);

        assert!(security_1 < security_2);
    }

    #[test]
    fn blowup_factor() {
        let field_extension = FieldExtension::Cubic;
        let base_field_bits = BaseElement::MODULUS_BITS;
        let fri_folding_factor = 8;
        let fri_remainder_max_degree = 127;
        let grinding_factor = 20;
        let blowup_factor = 8;
        let num_queries = 30;
        let collision_resistance = 128;
        let trace_length = 2_usize.pow(20);

        let mut options = ProofOptions::new(
            num_queries,
            blowup_factor,
            grinding_factor,
            field_extension,
            fri_folding_factor as usize,
            fri_remainder_max_degree as usize,
        );
        let security_1 =
            get_proven_security(&options, base_field_bits, trace_length, collision_resistance);

        let blowup_factor = 16;

        options = ProofOptions::new(
            num_queries,
            blowup_factor,
            grinding_factor,
            field_extension,
            fri_folding_factor as usize,
            fri_remainder_max_degree as usize,
        );
        let security_2 =
            get_proven_security(&options, base_field_bits, trace_length, collision_resistance);

        assert!(security_1 < security_2);
    }
}