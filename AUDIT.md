# Security review: credit_vault (2026-11-18)

**Scope.** `programs/credit_vault/src/*` (Anchor 0.31, Token-2022), the instruction flow in
`tests/credit_vault.ts`, and `scripts/merkle.ts` (the indexer's tree format). The off-chain
keeper, indexer and redemption service are not built yet and are out of scope.

**Method.**
1. A line-by-line manual pass.
2. Two independent reviews that did not see the manual findings: one on Solana and Anchor
   implementation security, one on economics and the trust model.
3. Every finding verified against the code. Confirmed code findings fixed, with a regression
   test each. Results:
   - `anchor test`: 27 of 27 full-flow tests pass on a local validator (three consecutive runs);
   - `cargo test`: 17 of 17 unit tests pass.

**Limits.** This is not a professional audit. There was no fuzzing, no formal verification and
no mainnet-fork testing. The program holds user funds, so get an independent professional audit
before mainnet.

## Summary

| ID | Severity | Finding | Status |
|---|---|---|---|
| C-1 | Critical | Auction price could be walked to about zero permanently | Fixed |
| C-2 | Critical | Dust buys compounded the auction price inside one transaction | Fixed |
| C-3 | Critical | The admin key alone could drain every vault instantly | Mitigated (timelock, root delay, settlement cap); residual trust below |
| H-1 | High | `initialize` could be front-run; a fake "USDC" was accepted | Fixed |
| H-2 | High | The on-chain credit cap never bound | Fixed |
| H-3 | High | The admin could reprice the live auction | Fixed |
| H-4 | High | The operator key could settle all pending money to itself | Mitigated (daily cap) |
| H-5 | High | Member rebate farmable through token-account owner reassignment | Fixed; residual economic note |
| M-1 | Medium | Token mint extensions and authorities not checked | Fixed |
| M-2 | Medium | Admin could lock user cancels (unbounded, live-read cancel window) | Fixed |
| M-3 | Medium | User could cancel mid-delivery, keeping both code and refund | Fixed |
| M-4 | Medium | One bad epoch could lock out all future roots | Fixed |
| M-5 | Medium | New fee lots joined a decayed auction and sold low | Fixed |
| M-6 | Medium | Pause had no time limit | Mitigated (auto-expiry) |
| L-1 | Low | Fee step-down ignored the per-transfer cap; same-rate calls postponed cuts | Fixed |
| L-2 | Low | One-step admin transfer | Fixed |
| L-3 | Low | Operator treasury not validated | Fixed |
| L-4 | Low | Member allowance restored with the live discount rate | Fixed |
| L-5 | Low | Redemption rent locked forever | Fixed |
| L-6 | Low | 30-day buckets allow up to 2x the monthly cap across a boundary | Accepted |
| I-1 to I-9 | Info | Trust and operations notes | See below |

## Findings and fixes

**C-1. Auction price walked to zero.**
- **Problem.** Anyone could call `restart_auction` while the fee vault was empty. Each call halved
  the price, and nothing limited how often it could happen between lots. Separately, the markup
  rounded down, so prices of about 4 base units or less could never rise again, and a markup of
  0 was allowed. A bot could take the price to 1 before launch, and every lot would then sell for
  fractions of a cent.
- **Fix.** `restart_auction` requires a non-empty fee vault, and the markup rounds up and must be
  at least 1%. The floor is now at least 50% and the duration at least 15 minutes. The admin can
  re-anchor the price through the timelocked config.
- **Tests.** "refuses to restart the auction while there is nothing to sell";
  `markup_rounds_up_so_tiny_prices_never_stick`; `params_bounds`.

**C-2. Dust-buy ratchet.**
- **Problem.** Every purchase, even 1 base unit, reset the auction to price x (1 + markup) and
  restarted the clock. Inside one transaction the price compounded: 40 dust buys multiplied it
  about 1,470x for 42 micro-USDC.
- **Fix.** Purchases never reset or mark up the running auction. A new lot's start is capped at
  the previous start plus the markup scaled by elapsed time, so in a single transaction it cannot
  rise at all.
- **Tests.** "dust buys inside one transaction cannot pump the price"; "sells the fee lot ... and
  does not reset the clock"; `same_transaction_ratchet_is_impossible`.

**C-3. Single-key drain.**
- **Problem.** `set_roles` took effect immediately. The admin could make itself indexer and
  operator, post a one-leaf root, redeem the whole reserve and fulfil it to its own treasury.
- **Fix.**
  - Every role and parameter change goes through `propose_config`, a 48-hour wait, then
    `apply_config` (or `cancel_config`).
  - Roots activate only after `root_delay_secs` and can be vetoed while they wait.
  - Settlement is capped per day by `max_fulfill_per_day`.
- **Residual.** A malicious admin can still appoint malicious roles, but with two days of public
  notice. The upgrade authority can do anything (I-1).
- **Test.** "role and parameter changes wait out the timelock".

**H-1. Initialize front-running.**
- **Problem.** Any signer could create the singleton config first. They could pass the real token
  mint (whose fee authority is the config PDA forever) and a fake 6-decimal USDC, then buy every
  fee lot with worthless tokens.
- **Fix.**
  - `initialize` requires the program's upgrade authority, checked against `ProgramData`.
  - USDC must live on the classic SPL Token program.
  - The operator treasury must be a USDC account outside the vaults.
- **Test.** "only the upgrade authority can initialize (no front-running)".

**H-2. Credit cap did not bind.**
- **Problem.** The post-payout check used a saturating subtraction and could never fail. The
  program did not tie the posted total to the tree's leaves, so a buggy or compromised tree could
  pay out beyond the declared total.
- **Fix.** Every redemption now requires `total_credit_consumed + amount <= active vested total`,
  which is itself at most the backing inflow.
- **Residual.** Whether the leaves add up to the total is still the indexer's job. A Merkle-sum
  tree would make that verifiable on-chain (I-8).
- **Test.** "total redemptions can never exceed the vested total, even if the leaves over-promise".

**H-3. Live repricing.**
- **Problem.** Changing duration or floor changed the running auction's price immediately.
- **Fix.** Each auction freezes its tuning when it starts. New tuning only arrives through the
  timelocked config and only applies to the next auction.

**H-4. Operator settles everything to itself.**
- **Problem.** `fulfill_redemption` accepted any receipt hash and had no limit.
- **Fix.** A daily settlement cap, set through the timelocked config.
- **Residual.** A compromised operator can take up to the cap per day. Keep the cap close to
  expected daily redemptions and keep the operator key separate from admin.
- **Test.** "settlement is capped per day ...".

**H-5. Rebate farming.**
- **Problem.** Membership read any token account the wallet owned. A non-associated Token-2022
  account can have its owner reassigned, so one bag could serve unlimited wallets for free.
- **Fix.** Membership requires the wallet's associated token account, which carries ImmutableOwner.
- **Residual.** Moving a bag between wallets (3% each hop) or flash-buying (about 6.5% round trip)
  still pays once `min_balance` is worth less than about $333 or $154. Keep `min_balance` worth at
  least $500 through the timelocked config, or move membership to a time-weighted balance in the
  Merkle leaf (I-9).
- **Test.** "membership must come from the wallet's own associated token account".

**M-1. Mint extensions.**
- **Problem.** Only the fee authority was checked. A permanent delegate could take fees from the
  fee vault, a transfer hook could block sales, a freeze authority or default-frozen state could
  freeze the vault, a confidential-transfer fee config has its own withdraw authority, and a live
  mint authority could inflate supply.
- **Fix.** Extensions are allowlisted: transfer fee, metadata pointer and token metadata. The mint
  must have no mint authority and no freeze authority.
- **Tests.** "refuses a token with a permanent delegate"; "refuses a token with a freeze
  authority"; "refuses a token whose fee authority is not the program".

**M-2. Cancel lock.**
- **Problem.** The cancel window had no upper bound and was read live, so setting it to i64::MAX
  blocked every user cancel.
- **Fix.** The window is at most 7 days, and each request stores its own `cancel_at`.
- **Test.** "a user cannot cancel before the request's own cancel time". The success path after
  the window needs clock warping and was not run on the local validator.

**M-3. Mid-delivery cancel.**
- **Problem.** Once the window opened, a user could cancel while the operator was buying the code.
- **Fix.** The operator calls `mark_processing` first. After that the user can cancel only once
  the request has been processing for 7 days.
- **Operating rule.** Mark processing, buy the code, fulfil on-chain, then reveal the code.
- **Test.** Covered in the settlement test.

**M-4. Epoch lockout.**
- **Problem.** Posting epoch u64::MAX would block every future root.
- **Fix.** The epoch must equal the active epoch plus 1.
- **Test.** "epochs must go up by exactly one".

**M-5. New lots selling at a decayed price.**
- **Problem.** The price kept decaying while there was nothing to sell, and collecting fees never
  restarted it.
- **Fix.** A fresh lot (vault empty, or new fees at least as large as the leftover) starts a new
  auction from the last sale price, inside the growth cap.
- **Test.** Inside the dust test.

**M-6. Pause.**
- **Problem.** No time limit.
- **Fix.** A pause expires after 7 days.
- **Residual.** The admin can pause again.

**L-1.** The fee rate must strictly fall, and `maximum_fee` cannot rise. Test: "the transfer fee
can only strictly go down".

**L-2.** `transfer_admin` plus `accept_admin`. Test: "admin handover takes two steps".

**L-3.** The treasury is checked at initialize and at apply.

**L-4.** `covered` is stored on each request. Asserted in the rebate test.

**L-5.** `close_redemption` returns rent for settled requests. Test: "the user can close a settled
request and get its rent back".

**L-6.** Accepted. At most one extra month of rebate allowance per wallet.

## Trust model after the fixes

- **Upgrade authority.** Can replace the program and so do anything (I-1).
- **Admin.**
  - With 48 hours' public notice: change indexer, operator, treasury and parameters within hard
    bounds; re-anchor the auction.
  - Immediately: pause new redemptions (expires after 7 days); veto a pending root; withdraw the
    ops vault; promote bonus into backing; lower the fee.
  - Cannot withdraw the reserve, the discount pool, pending money or the bonus vault.
- **Indexer.** Decides how vested credit is split between wallets, within total vested credit,
  which is at most backing inflow. Each root waits `root_delay_secs` and can be vetoed.
- **Operator.** Settles requests to the treasury up to the daily cap. Can mark processing or
  cancel (refund).
- **Users.** Can always cancel their own unsettled requests: pending ones after their own
  `cancel_at` (at most 7 days), processing ones after 7 days.

**Suggested production settings:**
- `root_delay_secs` 6-24 hours;
- `max_fulfill_per_day` near expected daily redemptions;
- auction floor at least 70% and duration at least 1 hour;
- `min_balance` worth at least $500;
- admin, indexer and operator as different keys, with admin and upgrade authority in a multisig
  (e.g. Squads with a time lock).

## Informational and decisions for the owner

- **I-1 (decision): upgrade authority.** Put it in a multisig with a time lock, or freeze the
  program after a professional audit. Publish a verifiable build.
- **I-2 (decision): no exit if the team disappears.** Backing USDC can only leave through operator
  fulfilment. A dead-man switch would let users claim outstanding credit as USDC by Merkle proof
  after N days with no fulfilment or root. It needs a product call, because it makes credit
  cash-redeemable in that case. Also publish every tree (Arweave or IPFS), so users can prove
  their credit.
- **I-3:** Circle can freeze USDC accounts, including the vaults. Accepted.
- **I-4:** The redemption service should scan `Redemption` accounts by status rather than rely
  only on event logs, which can be truncated.
- **I-5:** Item, platform, region and price are not validated on-chain. The operator must check
  the amount against its catalogue before buying a code.
- **I-6:** Proofs verify only against the active root, so clients must refresh proofs after each
  activation.
- **I-7:** The auction clears at least about 6% below the pool price, because of the transfer fee
  both ways, and it lags in sharp pumps: a new lot can rise only one markup per duration. Keep
  lots small and frequent (bundle harvest, collect and sale in one keeper transaction). A pool
  time-weighted price floor is a v2 option.
- **I-8:** A Merkle-sum tree would bind the posted total to the leaves on-chain.
- **I-9:** Membership by time-weighted balance in the Merkle leaf would end flash and hop farming.
- **Other notes:**
  - Cargo.lock resolves anchor-lang/anchor-spl 0.31.3 (a patch release); the CLI is 0.31.1.
  - A purchase refund needs the user's USDC account to exist.
