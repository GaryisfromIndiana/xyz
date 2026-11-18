//! Pure functions with no account access, so they can be unit-tested on the host.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::keccak;

use crate::errors::VaultError;
use crate::state::*;

/// Auction prices are micro-USDC per whole token, times this scale.
pub const PRICE_SCALE: u128 = 1_000_000;

const LEAF_PREFIX: &[u8] = &[0x00];
const NODE_PREFIX: &[u8] = &[0x01];

/// Leaf = keccak256(0x00 || wallet || cumulative_vested_le_u64).
/// The prefix stops a 64-byte internal node from being passed off as a leaf.
pub fn leaf_hash(user: &Pubkey, cumulative: u64) -> [u8; 32] {
    keccak::hashv(&[LEAF_PREFIX, user.as_ref(), &cumulative.to_le_bytes()]).to_bytes()
}

/// Internal nodes hash the sorted pair, so proofs need no left/right flags.
pub fn node_hash(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    if a <= b {
        keccak::hashv(&[NODE_PREFIX, a, b]).to_bytes()
    } else {
        keccak::hashv(&[NODE_PREFIX, b, a]).to_bytes()
    }
}

pub fn verify_proof(proof: &[[u8; 32]], root: &[u8; 32], leaf: [u8; 32]) -> bool {
    let mut computed = leaf;
    for sibling in proof {
        computed = node_hash(&computed, sibling);
    }
    computed == *root
}

pub fn validate_splits(s: &Splits) -> Result<()> {
    let sum = s.reserve_bps as u64 + s.discount_bps as u64 + s.ops_bps as u64;
    require!(sum == BPS, VaultError::InvalidSplits);
    require!(s.reserve_bps >= MIN_RESERVE_BPS, VaultError::InvalidSplits);
    require!(s.ops_bps <= MAX_OPS_BPS, VaultError::InvalidSplits);
    require!(s.burn_bps <= MAX_BURN_BPS, VaultError::InvalidSplits);
    Ok(())
}

pub fn validate_member(m: &MemberParams) -> Result<()> {
    require!(m.discount_bps <= MAX_MEMBER_DISCOUNT_BPS, VaultError::InvalidParams);
    require!(m.monthly_cap > 0, VaultError::InvalidParams);
    Ok(())
}

pub fn validate_tuning(t: &Tuning) -> Result<()> {
    require!(
        (MIN_AUCTION_SECS..=MAX_AUCTION_SECS).contains(&t.duration_secs),
        VaultError::InvalidParams
    );
    require!(
        (MIN_AUCTION_FLOOR_BPS..=MAX_AUCTION_FLOOR_BPS).contains(&t.floor_bps),
        VaultError::InvalidParams
    );
    require!(
        (MIN_RESTART_MARKUP_BPS..=MAX_RESTART_MARKUP_BPS).contains(&t.restart_markup_bps),
        VaultError::InvalidParams
    );
    Ok(())
}

pub fn validate_params(p: &Params) -> Result<()> {
    validate_splits(&p.splits)?;
    validate_member(&p.member)?;
    validate_tuning(&p.auction)?;
    require!(p.min_redemption >= MIN_REDEMPTION_FLOOR, VaultError::InvalidParams);
    require!(
        (MIN_CANCEL_AFTER_SECS..=MAX_CANCEL_AFTER_SECS).contains(&p.cancel_after_secs),
        VaultError::InvalidParams
    );
    require!((0..=MAX_ROOT_DELAY_SECS).contains(&p.root_delay_secs), VaultError::InvalidParams);
    require!(p.max_fulfill_per_day > 0, VaultError::InvalidParams);
    Ok(())
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct ProceedsSplit {
    pub reserve: u64,
    pub discount: u64,
    pub ops: u64,
}

/// Divide auction proceeds. Rounding dust goes to the reserve, never to ops.
pub fn split_proceeds(amount: u64, s: &Splits) -> ProceedsSplit {
    let discount = ((amount as u128 * s.discount_bps as u128) / BPS as u128) as u64;
    let ops = ((amount as u128 * s.ops_bps as u128) / BPS as u128) as u64;
    ProceedsSplit {
        reserve: amount - discount - ops,
        discount,
        ops,
    }
}

pub fn burn_amount(collected: u64, burn_bps: u16) -> u64 {
    ((collected as u128 * burn_bps as u128) / BPS as u128) as u64
}

/// Linear decay from `start_price` to `start_price * floor_bps` over the running auction's
/// duration, then flat at the floor. Uses the tuning frozen when the auction started.
pub fn auction_price(a: &Auction, now: i64) -> u64 {
    let floor = floor_price(a);
    if now <= a.started_at {
        return a.start_price;
    }
    let elapsed = (now - a.started_at) as u128;
    let duration = a.tuning.duration_secs as u128;
    if elapsed >= duration {
        return floor;
    }
    let drop = (a.start_price - floor) as u128 * elapsed / duration;
    a.start_price - drop as u64
}

pub fn floor_price(a: &Auction) -> u64 {
    let f = (a.start_price as u128 * a.tuning.floor_bps as u128 / BPS as u128) as u64;
    f.max(1)
}

/// USDC owed for `token_amount` base units at `price`, rounded up so the vault never undersells.
pub fn usdc_for_tokens(token_amount: u64, price: u64, token_decimals: u8) -> Result<u64> {
    let denom = 10u128
        .checked_pow(token_decimals as u32)
        .and_then(|d| d.checked_mul(PRICE_SCALE))
        .ok_or(VaultError::MathOverflow)?;
    let num = (token_amount as u128)
        .checked_mul(price as u128)
        .ok_or(VaultError::MathOverflow)?;
    let due = num.div_ceil(denom);
    u64::try_from(due).map_err(|_| error!(VaultError::MathOverflow))
}

/// Price marked up by `markup_bps`, rounded up, so it always rises by at least one unit
/// (a rounded-down markup let tiny prices get stuck: audit).
pub fn next_start_price(clearing: u64, markup_bps: u16) -> u64 {
    let num = clearing as u128 * (BPS as u128 + markup_bps as u128);
    let p = num.div_ceil(BPS as u128);
    (p.min(u64::MAX as u128) as u64).max(1)
}

/// Highest start price a new lot may open at: the previous start plus the markup, scaled by how
/// much of a full auction has elapsed. Within one transaction (elapsed 0) the price cannot rise
/// at all, which kills the dust-buy ratchet; after a full duration it may rise by one markup.
pub fn growth_cap(a: &Auction, now: i64) -> u64 {
    let elapsed = (now - a.started_at).max(0) as u128;
    let duration = a.tuning.duration_secs.max(1) as u128;
    let frac = elapsed.min(duration);
    let markup = a.tuning.restart_markup_bps as u128;
    let cap = a.start_price as u128 * (BPS as u128 * duration + markup * frac) / (BPS as u128 * duration);
    (cap.min(u64::MAX as u128) as u64).max(1)
}

/// Start price for a new lot (the fee vault was empty and fees just arrived): restart high from
/// the last sale rather than from wherever the price decayed to while there was nothing to sell,
/// but never above the growth cap.
pub fn new_lot_start(a: &Auction, now: i64) -> u64 {
    let current = auction_price(a, now);
    let from_clear = if a.last_clear_price > 0 {
        next_start_price(a.last_clear_price, a.tuning.restart_markup_bps)
    } else {
        0
    };
    current.max(from_clear).min(growth_cap(a, now)).max(1)
}

/// Member rebate on a purchase at face: `discount_bps` of the price, limited by what the
/// discount pool holds and by the wallet's remaining monthly allowance. A drained pool or a
/// used-up allowance means a smaller or zero rebate, never a failed purchase.
pub fn member_rebate(price: u64, discount_bps: u16, pool_balance: u64, allowance_left: u64) -> u64 {
    let eligible = price.min(allowance_left);
    let full = (eligible as u128 * discount_bps as u128 / BPS as u128) as u64;
    full.min(pool_balance)
}

/// Credit outstanding = vested credit in the latest root minus credit already spent.
pub fn credit_outstanding(root_total_vested: u64, consumed: u64) -> u64 {
    root_total_vested.saturating_sub(consumed)
}

pub fn month_index(now: i64) -> i64 {
    now.div_euclid(MONTH_SECS)
}

pub fn day_index(now: i64) -> i64 {
    now.div_euclid(DAY_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_tree(leaves: &[[u8; 32]]) -> (Vec<Vec<[u8; 32]>>, [u8; 32]) {
        let mut levels = vec![leaves.to_vec()];
        while levels.last().unwrap().len() > 1 {
            let prev = levels.last().unwrap();
            let mut next = Vec::new();
            for pair in prev.chunks(2) {
                if pair.len() == 2 {
                    next.push(node_hash(&pair[0], &pair[1]));
                } else {
                    next.push(pair[0]);
                }
            }
            levels.push(next);
        }
        let root = levels.last().unwrap()[0];
        (levels, root)
    }

    fn proof_for(levels: &[Vec<[u8; 32]>], mut idx: usize) -> Vec<[u8; 32]> {
        let mut proof = Vec::new();
        for level in &levels[..levels.len() - 1] {
            let sib = idx ^ 1;
            if sib < level.len() {
                proof.push(level[sib]);
            }
            idx /= 2;
        }
        proof
    }

    #[test]
    fn merkle_round_trip_and_rejects_wrong_amount() {
        let users: Vec<Pubkey> = (0..5).map(|_| Pubkey::new_unique()).collect();
        let amounts = [10_000_000u64, 25_000_000, 0, 7_500_000, 1];
        let leaves: Vec<[u8; 32]> = users
            .iter()
            .zip(amounts)
            .map(|(u, a)| leaf_hash(u, a))
            .collect();
        let (levels, root) = build_tree(&leaves);
        for i in 0..users.len() {
            let proof = proof_for(&levels, i);
            assert!(verify_proof(&proof, &root, leaf_hash(&users[i], amounts[i])));
            assert!(!verify_proof(&proof, &root, leaf_hash(&users[i], amounts[i] + 1)));
        }
        let proof0 = proof_for(&levels, 0);
        assert!(!verify_proof(&proof0, &root, leaf_hash(&users[1], amounts[0])));
    }

    /// Same vector as scripts/merkle.ts computes, so the indexer and the program agree byte for byte.
    #[test]
    fn matches_typescript_tree_vector() {
        let a = Pubkey::new_from_array([1u8; 32]);
        let b = Pubkey::new_from_array([2u8; 32]);
        let c = Pubkey::new_from_array([3u8; 32]);
        let hex = |h: [u8; 32]| h.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(
            hex(leaf_hash(&a, 10_000_000)),
            "858e6e8d0053b55d4deded7fe45b448c34e3b3609357fd36ef21ee1711924244"
        );
        let leaves = [leaf_hash(&a, 10_000_000), leaf_hash(&b, 25_000_000), leaf_hash(&c, 7)];
        let (levels, root) = build_tree(&leaves);
        assert_eq!(hex(root), "b4ecce2ab54934e6024b08d0eb5fcbf7563a91557713030a84126c86cf08cb9f");
        for (i, (u, amt)) in [(a, 10_000_000u64), (b, 25_000_000), (c, 7)].iter().enumerate() {
            assert!(verify_proof(&proof_for(&levels, i), &root, leaf_hash(u, *amt)));
        }
    }

    #[test]
    fn leaf_and_node_domains_differ() {
        let u = Pubkey::new_unique();
        let a = leaf_hash(&u, 5);
        let b = leaf_hash(&Pubkey::new_unique(), 6);
        let n = node_hash(&a, &b);
        assert_ne!(n, a);
        assert_eq!(node_hash(&a, &b), node_hash(&b, &a));
    }

    fn spec_splits() -> Splits {
        Splits { burn_bps: 667, reserve_bps: 7_500, discount_bps: 1_071, ops_bps: 1_429 }
    }

    #[test]
    fn spec_splits_are_valid_and_match_the_tax() {
        let s = spec_splits();
        validate_splits(&s).unwrap();
        // 3% tax on $1,000,000 of volume = $30,000 of fee tokens (valued in USDC here).
        let fees = 30_000_000_000u64;
        let burned = burn_amount(fees, s.burn_bps);
        let sold = fees - burned;
        let p = split_proceeds(sold, &s);
        assert_eq!(p.reserve + p.discount + p.ops, sold);
        // ~2.1% / 0.3% / 0.4% / 0.2% of volume, within rounding of the bps grid.
        assert!((p.reserve as i64 - 21_000_000_000).abs() < 30_000_000);
        assert!((p.discount as i64 - 3_000_000_000).abs() < 30_000_000);
        assert!((p.ops as i64 - 4_000_000_000).abs() < 30_000_000);
        assert!((burned as i64 - 2_000_000_000).abs() < 30_000_000);
    }

    #[test]
    fn splits_bounds_enforced() {
        let mut s = spec_splits();
        s.reserve_bps = 6_000;
        s.ops_bps = 2_929;
        assert!(validate_splits(&s).is_err());
        let mut s = spec_splits();
        s.discount_bps += 1;
        assert!(validate_splits(&s).is_err());
        let mut s = spec_splits();
        s.burn_bps = 2_500;
        assert!(validate_splits(&s).is_err());
    }

    #[test]
    fn split_dust_goes_to_reserve() {
        // 6 micro-USDC: both minor shares floor to zero, everything lands in the reserve.
        let p = split_proceeds(6, &spec_splits());
        assert_eq!((p.reserve, p.discount, p.ops), (6, 0, 0));
        // For any amount the parts sum exactly, and the reserve never gets less than its bps share.
        for amount in [1u64, 7, 99, 1_234_567, 30_000_000_000] {
            let p = split_proceeds(amount, &spec_splits());
            assert_eq!(p.reserve + p.discount + p.ops, amount);
            assert!(p.reserve as u128 * BPS as u128 >= amount as u128 * 7_500);
        }
    }

    fn auction(start: u64, started_at: i64, last_clear: u64) -> Auction {
        Auction {
            start_price: start,
            started_at,
            tuning: Tuning { duration_secs: 1_000, floor_bps: 5_000, restart_markup_bps: 2_000 },
            last_clear_price: last_clear,
        }
    }

    #[test]
    fn auction_decays_linearly_then_floors() {
        let a = auction(1_000_000, 100, 0);
        assert_eq!(auction_price(&a, 50), 1_000_000);
        assert_eq!(auction_price(&a, 100), 1_000_000);
        assert_eq!(auction_price(&a, 600), 750_000);
        assert_eq!(auction_price(&a, 1_100), 500_000);
        assert_eq!(auction_price(&a, 9_999), 500_000);
        assert_eq!(next_start_price(750_000, 2_000), 900_000);
    }

    #[test]
    fn markup_rounds_up_so_tiny_prices_never_stick() {
        for p in [1u64, 2, 3, 4, 5] {
            assert!(next_start_price(p, 100) > p, "price {p} stuck");
            assert!(next_start_price(p, 2_000) > p, "price {p} stuck");
        }
    }

    #[test]
    fn same_transaction_ratchet_is_impossible() {
        // A lot cleared at the start price in the same second the auction started:
        // the next lot may not open any higher.
        let a = auction(1_000_000, 500, 1_000_000);
        assert_eq!(growth_cap(&a, 500), 1_000_000);
        assert_eq!(new_lot_start(&a, 500), 1_000_000);
        // Repeating it any number of times keeps the price flat.
        let mut b = a;
        for _ in 0..100 {
            let next = new_lot_start(&b, 500);
            b.start_price = next;
            b.last_clear_price = next;
        }
        assert_eq!(b.start_price, 1_000_000);
    }

    #[test]
    fn new_lot_can_rise_one_markup_per_full_duration() {
        let a = auction(1_000_000, 0, 1_000_000);
        assert_eq!(growth_cap(&a, 500), 1_100_000);
        assert_eq!(growth_cap(&a, 1_000), 1_200_000);
        assert_eq!(growth_cap(&a, 50_000), 1_200_000);
        // After decaying with nothing to sell, a new lot restarts from the last sale, not the floor.
        let decayed = auction(1_000_000, 0, 600_000);
        assert_eq!(auction_price(&decayed, 5_000), 500_000);
        assert_eq!(new_lot_start(&decayed, 5_000), 720_000);
    }

    #[test]
    fn params_bounds() {
        let ok = Params {
            splits: spec_splits(),
            member: MemberParams { discount_bps: 1_000, min_balance: 1, monthly_cap: 100_000_000 },
            min_redemption: 10_000_000,
            cancel_after_secs: 3_600,
            root_delay_secs: 3_600,
            max_fulfill_per_day: 50_000_000,
            auction: Tuning { duration_secs: 3_600, floor_bps: 5_000, restart_markup_bps: 2_000 },
        };
        validate_params(&ok).unwrap();
        let mut p = ok;
        p.cancel_after_secs = i64::MAX;
        assert!(validate_params(&p).is_err());
        let mut p = ok;
        p.auction.restart_markup_bps = 0;
        assert!(validate_params(&p).is_err());
        let mut p = ok;
        p.auction.floor_bps = 1_000;
        assert!(validate_params(&p).is_err());
        let mut p = ok;
        p.auction.duration_secs = 60;
        assert!(validate_params(&p).is_err());
        let mut p = ok;
        p.root_delay_secs = 2 * DAY_SECS;
        assert!(validate_params(&p).is_err());
    }

    #[test]
    fn usdc_due_rounds_up() {
        // 1,000 whole tokens (6 decimals) at $0.0015 each = $1.50.
        let price = 1_500 * PRICE_SCALE as u64; // 1,500 micro-USDC per token
        assert_eq!(usdc_for_tokens(1_000_000_000, price, 6).unwrap(), 1_500_000);
        // One base unit still costs at least one micro-USDC.
        assert_eq!(usdc_for_tokens(1, price, 6).unwrap(), 1);
    }

    #[test]
    fn member_rebate_degrades_gracefully() {
        // $20 at 10% off, pool and allowance ample.
        assert_eq!(member_rebate(20_000_000, 1_000, 1_000_000_000, 100_000_000), 2_000_000);
        // Pool nearly empty: rebate shrinks to what is there.
        assert_eq!(member_rebate(20_000_000, 1_000, 500_000, 100_000_000), 500_000);
        // Pool empty: no rebate, purchase still goes through at face.
        assert_eq!(member_rebate(20_000_000, 1_000, 0, 100_000_000), 0);
        // Only $5 of monthly allowance left: rebate on $5 only.
        assert_eq!(member_rebate(20_000_000, 1_000, 1_000_000_000, 5_000_000), 500_000);
        assert_eq!(member_rebate(20_000_000, 1_000, 1_000_000_000, 0), 0);
    }

    #[test]
    fn spec_ratios_hold_at_every_fee_step() {
        // The split is a ratio of the fee, so the shares of volume scale with the step-down.
        let s = spec_splits();
        for (fee_bps, reserve_share_bps_x100) in [(300u64, 21_000u64), (200, 14_000), (100, 7_000)] {
            let volume = 1_000_000_000_000u64; // $1M in micro-USDC
            let fees = volume * fee_bps / 10_000;
            let sold = fees - burn_amount(fees, s.burn_bps);
            let reserve = split_proceeds(sold, &s).reserve;
            // reserve / volume ~= 0.7 * fee, in hundredths of a bp
            let got = (reserve as u128 * 1_000_000 / volume as u128) as u64;
            assert!((got as i64 - reserve_share_bps_x100 as i64).abs() < 30, "fee {fee_bps}: {got}");
        }
    }

    #[test]
    fn outstanding_never_negative() {
        assert_eq!(credit_outstanding(100, 30), 70);
        assert_eq!(credit_outstanding(30, 100), 0);
    }

    #[test]
    fn month_index_rolls_every_30_days() {
        assert_eq!(month_index(0), 0);
        assert_eq!(month_index(MONTH_SECS - 1), 0);
        assert_eq!(month_index(MONTH_SECS), 1);
    }
}
