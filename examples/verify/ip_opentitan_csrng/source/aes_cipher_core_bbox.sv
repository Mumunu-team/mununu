// LOCAL (mununu): black-box stub of OpenTitan aes_cipher_core — the port list is the upstream header
// (hw/ip/aes/rtl/aes_cipher_core.sv) verbatim; the body is deliberately absent and the module is
// marked (* blackbox *), so the lift cuts every output to a free input: a declared
// over-approximation (the module's datapath is not modelled).
(* blackbox *)
module aes_cipher_core import aes_pkg::*;
#(
  parameter bit          AES192Enable         = 1,
  parameter bit          CiphOpFwdOnly        = 0,
  parameter bit          SecMasking           = 1,
  parameter sbox_impl_e  SecSBoxImpl          = SBoxImplDom,
  parameter bit          SecAllowForcingMasks = 0,
  parameter bit          SecSkipPRNGReseeding = 0,
  parameter int unsigned EntropyWidth         = edn_pkg::ENDPOINT_BUS_WIDTH,

  localparam int         NumShares            = SecMasking ? 2 : 1, // derived parameter

  parameter masking_lfsr_seed_t RndCnstMaskingLfsrSeed = RndCnstMaskingLfsrSeedDefault,
  parameter masking_lfsr_perm_t RndCnstMaskingLfsrPerm = RndCnstMaskingLfsrPermDefault
) (
  input  logic                        clk_i,
  input  logic                        rst_ni,

  // Input handshake signals
  input  sp2v_e                       in_valid_i,
  output sp2v_e                       in_ready_o,

  // Output handshake signals
  output sp2v_e                       out_valid_o,
  input  sp2v_e                       out_ready_i,

  // Control and sync signals
  input  logic                        cfg_valid_i, // Used for gating assertions only.
  input  ciph_op_e                    op_i,
  input  key_len_e                    key_len_i,
  input  sp2v_e                       crypt_i,
  output sp2v_e                       crypt_o,
  input  sp2v_e                       dec_key_gen_i,
  output sp2v_e                       dec_key_gen_o,
  input  logic                        prng_reseed_i,
  output logic                        prng_reseed_o,
  input  logic                        key_clear_i,
  output logic                        key_clear_o,
  input  logic                        data_out_clear_i, // Re-use the cipher core muxes.
  output logic                        data_out_clear_o,
  input  logic                        alert_fatal_i,
  output logic                        alert_o,

  // Pseudo-random data for register clearing
  input  logic        [3:0][3:0][7:0] prd_clearing_state_i [NumShares],
  input  logic            [7:0][31:0] prd_clearing_key_i [NumShares],

  // Masking PRNG
  input  logic                        force_masks_i, // Useful for SCA only.
  output logic        [3:0][3:0][7:0] data_in_mask_o,
  output logic                        entropy_req_o,
  input  logic                        entropy_ack_i,
  input  logic     [EntropyWidth-1:0] entropy_i,

  // I/O data & initial key
  input  logic        [3:0][3:0][7:0] state_init_i [NumShares],
  input  logic            [7:0][31:0] key_init_i [NumShares],
  output logic        [3:0][3:0][7:0] state_o [NumShares]
);
endmodule
