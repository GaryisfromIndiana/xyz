# GTA 6 credit vault (Solana, Anchor)

Hold the token, accrue USD credit backed 1:1 by USDC, redeem it for GTA 6 (game, Ultimate
upgrade) as PlayStation or Xbox wallet credit now, and for GTA 6 currency when Rockstar opens a
store. The product spec and research are kept separately by the project owner.

## How money moves

```
trade --3% Token-2022 transfer fee--> withheld in token accounts
  harvest_withheld_tokens_to_mint (Token-2022, permissionless)
  collect_fees (permissionless)  -> fee_vault; burn 6.67%
  buy_fees (permissionless Dutch auction) -> USDC split:
        75%    reserve_vault   (backing; the only source of credit)
        10.71% discount_vault  (member rebates)
        14.29% ops_vault       (operations; the only admin-withdrawable money)
indexer: hourly TWAB, 30-day vesting, pro-rata forfeits -> post_root(merkle root, total_vested)
user:    request_redemption(proof) -> pending_vault -> operator buys code -> fulfill_redemption
anyone:  buy_credit at face (USDC) -> members get 10% back from discount_vault
```

At a 3% fee these ratios reproduce the spec's 2.1% reserve, 0.3% discount pool, 0.4% ops and
0.2% burn of volume. Because they are ratios of the fee, they keep working after the fee
steps down to 2% and 1% (design review change 1).

## Invariants the program enforces

The full audit is in AUDIT.md.

1. **Only the program can take the tax.** `initialize` refuses any token mint that fails these:
   - the withheld-fee authority is the config PDA;
   - the fee-rate authority is the PDA or none;
   - there is no mint authority (fixed supply) and no freeze authority;
   - every extension is on the allowlist: transfer fee, metadata pointer, token metadata.
2. **Only the upgrade authority can initialize,** so nobody can front-run the singleton config.
   USDC must be on the classic SPL Token program.
3. **Total spending never exceeds vested credit, and vested credit never exceeds backing.**
   - `post_root` rejects a total above the cumulative USDC in backing.
   - Every redemption requires total spent + amount <= active vested total, whatever the tree's
     leaves say.
   - The reserve balance is re-checked after each payout.
4. **Roots wait before they count.** A posted root activates after `root_delay_secs`, and the
   admin can veto it while it waits. Epochs must go up by exactly one.
5. **No admin path out of the reserve, the discount pool, pending money or bonus.**
   - Only `withdraw_ops` moves money out at the admin's request.
   - Bonus money becomes backing only through the one-way `promote_bonus`.
6. **Role and parameter changes wait 48 hours** (`propose_config`, then `apply_config`). New
   auction tuning applies only to the next auction, never the live one.
7. **The auction cannot be pumped or collapsed.**
   - Purchases never reset or mark up the running auction.
   - A new lot opens at most one markup above the previous start per full duration, and not at
     all within one transaction.
   - `restart_auction` needs an unsold lot.
   - The markup rounds up.
8. **The fee only goes down.** `set_transfer_fee` needs a strictly lower rate and no higher cap.
9. **Settlement is capped per day.** That bounds what a compromised operator key can take.
10. **Users can always get unsettled money back:**
    - pending requests after their own stored `cancel_at` (at most 7 days);
    - requests the operator marked as processing, after 7 days.
11. **A drained discount pool never blocks a purchase or touches backing.**
12. **Rebates need the wallet's own associated token account,** so one bag cannot be reassigned
    between wallets.
13. **Pause expires on its own** after 7 days.

## Instructions

| group | instruction | who |
|---|---|---|
| setup | `initialize` (upgrade authority only), `init_vaults_a`, `init_vaults_b` | admin, once |
| admin, timelocked | `propose_config`, `apply_config` (after 48h), `cancel_config` | admin |
| admin, immediate | `set_paused` (auto-expires), `veto_root`, `set_transfer_fee` (down only), `withdraw_ops`, `promote_bonus`, `transfer_admin` | admin |
| admin, immediate | `accept_admin` | new admin |
| fees | `collect_fees`, `buy_fees`, `restart_auction` | anyone |
| money in | `donate_to_reserve` (counts as backing), `deposit_bonus` | anyone |
| credit | `post_root` | indexer |
| credit | `activate_root` | anyone, after the delay |
| credit | `request_redemption`, `buy_credit` | user |
| settle | `mark_processing`, `fulfill_redemption` (daily cap, receipt hash onchain) | operator |
| settle | `cancel_redemption` | operator any time; user after `cancel_at` |
| settle | `close_redemption` (rent back) | user, once settled |

Pause blocks `request_redemption` and `buy_credit`. Settlement and cancels keep working.

**Operator rule.** `mark_processing`, buy the code, `fulfill_redemption`, then reveal the code.

## Off-chain services (v1)

- **Keeper.** Sends `harvest_withheld_tokens_to_mint` plus `collect_fees` on a schedule. The
  auction is permissionless, so the keeper needs no special key; anyone can buy the fee lot
  and sell it on the pool.
- **Indexer.** Builds the credit tree with `scripts/merkle.ts`, whose hashing is pinned to the
  program by a shared test vector. Its job:
  - hourly time-weighted balances, excluding pool, team and exchange accounts;
  - credit per hour = reserve inflow that hour x wallet TWAB / eligible TWAB;
  - linear vesting over 30 days;
  - **pro-rata forfeits**: selling a fraction f of the bag forfeits fraction f of unvested
    credit, redistributed to stayers (design review change 3);
  - posting the root with the cumulative vested total. Publish every input so anyone can
    recompute the root.
- **Redemption service.** Watches `RedemptionRequested`. It buys the code through the
  Bitrefill Business API or Tango, reveals it once to the signing wallet, and calls
  `fulfill_redemption` with the receipt hash.

## Deferred to v2

- **Credit notes,** the tradeable $1 game-credit note. The design review advised waiting until a non-holder
  buy side is proven. Config has 64 reserved bytes for the extra fields.
- **Paying the ops share in the token** instead of USDC, to cut sell flow.
- **Reserve yield,** a lending or T-bill vault for idle backing. For now yield arrives via
  `deposit_bonus`.

## Status (2026-11-18, after the audit)

- `anchor build` passes. It produces `target/deploy/credit_vault.so` (about 752 KB), the IDL and
  the TypeScript types.
- `anchor test`: **27 of 27 full-flow tests pass** on a local validator, the same on three
  consecutive runs. They include a regression test for each audit fix.
- `cargo test`: 17 unit tests pass. They include the auction anti-ratchet maths and the Merkle
  vector shared with `scripts/merkle.ts`.
- `tsc --noEmit` passes.
- **Self-audit done** (AUDIT.md): 3 critical, 5 high, 6 medium and 6 low findings, all fixed or
  mitigated. **Not a professional audit**; get one before mainnet.
- Deploy cost: the program account needs roughly 5.2 SOL of rent at this size. Rent is refundable
  if the program is closed.

## Setup on your machine

```bash
# 1. Solana CLI (Agave), official installer
sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"

# 2. Anchor 0.31.1 via avm (run as plain commands, not prefixed with "!")
cargo install --git https://github.com/solana-foundation/anchor avm --force
avm install 0.31.1 && avm use 0.31.1

# 3. Build and test. Anchor.toml pins Solana 2.1.0, so anchor switches the active release to it.
cd gta6-credit-vault   # or wherever you cloned it
npm install
anchor build
solana-keygen new --no-bip39-passphrase --silent --outfile tests/localnet.keypair.json
anchor test --skip-build --provider.wallet tests/localnet.keypair.json
```

**Toolchain notes**
- Anchor 0.31.1 builds with the Solana 2.1 SBF toolchain, which ships Cargo and rustc 1.79.
  So the program declares `rust-version = "1.79"`. `.cargo/config.toml` makes newer host Cargo
  pick 1.79-compatible dependencies, and `blake3` is pinned to 1.5.5 in `Cargo.lock`, because
  1.8.x pulls in crates on the 2024 edition.
- If you run `cargo update`, re-pin with `cargo update -p blake3 --precise 1.5.5`.
- The local test wallet (`tests/localnet.keypair.json`) is a throwaway key for the local
  validator and is git-ignored. Use a real wallet for devnet.

The program keypair is `target/deploy/credit_vault-keypair.json`. It is git-ignored, so back
it up: it controls the program ID `FxpPCZhoY2ecs9g9w5M9VK7m5fMW8mWF28hjhrE8QGtY`.

Anchor 1.x is the current stable line (1.2.2). This repo pins 0.31.1, whose API it was
written against. Upgrading is a separate pass.

## Launch-route note

`scripts/create-mint.ts` is the self-launch route. It mints a Token-2022 token with a 3% fee,
both fee authorities set to the config PDA, fixed supply and no freeze authority. You then seed
a DAMM v2 or Raydium CPMM pool.

Launching on Meteora DBC is different: DBC creates the mint and sets the fee authorities to the
partner or creator. The creator would then have to hand both authorities to the config PDA
with Token-2022 `SetAuthority` before `initialize`. Confirm DBC allows that (open question 2).

## Open questions

1. Can a DAMM v2 or Raydium CPMM pool collect a 2-3% creator fee in SOL? That would remove the
   harvest-and-sell step entirely (design review check e).
2. Meteora DBC: can the creator move the withheld-fee and fee-config authorities to a PDA after
   creation?
3. What does the Bitrefill Business API offer: volume pricing, PlayStation and Xbox region
   coverage, and daily limits?
4. How to deliver the code: encrypt it to the wallet, or show it on a signed session page?
5. What is the member minimum balance? Set it high enough that buying in and selling out (6% in
   taxes) costs more than the $10 monthly rebate.
