import * as anchor from "@coral-xyz/anchor";
import { BN, Program } from "@coral-xyz/anchor";
import { Keypair, LAMPORTS_PER_SOL, PublicKey, SystemProgram, Transaction } from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  createAccount,
  createAssociatedTokenAccountIdempotent,
  createHarvestWithheldTokensToMintInstruction,
  createMint,
  getAccount,
  getAssociatedTokenAddressSync,
  mintTo,
  transferChecked,
} from "@solana/spl-token";
import { assert } from "chai";
import { CreditTree } from "../scripts/merkle";
import { createFeeMint } from "../scripts/create-mint";

const DEC = 6;
const ONE = 10n ** BigInt(DEC);
const U64_MAX = new BN("18446744073709551615");
const usd = (n: number) => new BN(Math.round(n * 1_000_000));
const US: number[] = [85, 83];
const BPF_UPGRADEABLE = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");
const ROOT_DELAY = 2; // seconds, so the tests can watch a root activate
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function expectErr(p: Promise<unknown>, code: string) {
  let threw = false;
  try {
    await p;
  } catch (e: any) {
    threw = true;
    const text = `${e?.error?.errorCode?.code ?? ""} ${e?.message ?? ""} ${JSON.stringify(e?.logs ?? [])}`;
    assert.include(text, code, `expected ${code}, got: ${text.slice(0, 300)}`);
  }
  assert.isTrue(threw, `expected the call to fail with ${code}`);
}

describe("credit_vault", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = (anchor.workspace.CreditVault ?? anchor.workspace.creditVault) as Program;
  const connection = provider.connection;
  const admin = (provider.wallet as anchor.Wallet).payer;
  const indexer = Keypair.generate();
  const operator = Keypair.generate();
  const alice = Keypair.generate();
  const bob = Keypair.generate();
  const buyer = Keypair.generate();
  const attacker = Keypair.generate();
  const newAdmin = Keypair.generate();
  const acct = program.account as any;

  const pda = (...seeds: (Buffer | Uint8Array)[]) => PublicKey.findProgramAddressSync(seeds, program.programId)[0];
  const config = pda(Buffer.from("config"));
  const feeVault = pda(Buffer.from("fee_vault"));
  const reserveVault = pda(Buffer.from("reserve_vault"));
  const discountVault = pda(Buffer.from("discount_vault"));
  const opsVault = pda(Buffer.from("ops_vault"));
  const pendingVault = pda(Buffer.from("pending_vault"));
  const bonusVault = pda(Buffer.from("bonus_vault"));
  const programData = PublicKey.findProgramAddressSync([program.programId.toBuffer()], BPF_UPGRADEABLE)[0];
  const userState = (u: PublicKey) => pda(Buffer.from("user"), u.toBuffer());
  const redemption = (u: PublicKey, id: number) =>
    pda(Buffer.from("redemption"), u.toBuffer(), new BN(id).toArrayLike(Buffer, "le", 8));

  let usdc: PublicKey;
  let token: PublicKey;
  let treasury: PublicKey;
  const ataUsdc = (o: PublicKey) => getAssociatedTokenAddressSync(usdc, o, false, TOKEN_PROGRAM_ID);
  const ataTok = (o: PublicKey) => getAssociatedTokenAddressSync(token, o, false, TOKEN_2022_PROGRAM_ID);
  // Read at the provider's own commitment so a read right after a transaction sees it.
  const commitment = provider.opts.commitment ?? "processed";
  const bal = async (a: PublicKey, prog = TOKEN_PROGRAM_ID) => (await getAccount(connection, a, commitment, prog)).amount;
  const cfg = async (): Promise<any> => acct.config.fetch(config);

  const params = {
    splits: { burnBps: 667, reserveBps: 7500, discountBps: 1071, opsBps: 1429 },
    member: { discountBps: 1000, minBalance: new BN((1_000_000n * ONE).toString()), monthlyCap: usd(100) },
    minRedemption: usd(10),
    cancelAfterSecs: new BN(3600),
    rootDelaySecs: new BN(ROOT_DELAY),
    maxFulfillPerDay: usd(50),
    auction: { durationSecs: new BN(3600), floorBps: 5000, restartMarkupBps: 2000 },
  };
  // $0.001 per token = 1,000 micro-USDC per whole token x PRICE_SCALE (1e6).
  const initArgs = () => ({ indexer: indexer.publicKey, operator: operator.publicKey, params, startPrice: new BN(1_000 * 1_000_000) });
  const initAccounts = (signer: PublicKey, mint: PublicKey) => ({
    admin: signer,
    config,
    tokenMint: mint,
    usdcMint: usdc,
    operatorTreasury: treasury,
    program: program.programId,
    programData,
    tokenProgram: TOKEN_PROGRAM_ID,
    token2022Program: TOKEN_2022_PROGRAM_ID,
    systemProgram: SystemProgram.programId,
  });

  const aliceTokens = 10_000_000n * ONE;
  const bobTokens = 5_000_000n * ONE;
  let feesWithheld = 0n;

  before(async () => {
    for (const k of [indexer, operator, alice, bob, buyer, attacker, newAdmin]) {
      const sig = await connection.requestAirdrop(k.publicKey, 10 * LAMPORTS_PER_SOL);
      await connection.confirmTransaction(sig, "confirmed");
    }
    usdc = await createMint(connection, admin, admin.publicKey, null, DEC, undefined, undefined, TOKEN_PROGRAM_ID);
    token = await createFeeMint(connection, admin, { decimals: DEC, supplyWhole: 1_000_000_000n, feeBps: 300, programId: program.programId });
    treasury = await createAssociatedTokenAccountIdempotent(connection, admin, usdc, operator.publicKey, undefined, TOKEN_PROGRAM_ID);
    for (const [who, amount] of [[buyer, 100_000n], [bob, 1_000n], [alice, 0n]] as const) {
      const a = await createAssociatedTokenAccountIdempotent(connection, admin, usdc, who.publicKey, undefined, TOKEN_PROGRAM_ID);
      if (amount > 0n) await mintTo(connection, admin, usdc, a, admin, amount * ONE, [], undefined, TOKEN_PROGRAM_ID);
    }
    for (const who of [alice, bob, buyer]) {
      await createAssociatedTokenAccountIdempotent(connection, admin, token, who.publicKey, undefined, TOKEN_2022_PROGRAM_ID);
    }
    for (const [who, amount] of [[alice, aliceTokens], [bob, bobTokens]] as const) {
      await transferChecked(connection, admin, ataTok(admin.publicKey), token, ataTok(who.publicKey), admin, amount, DEC, [], undefined, TOKEN_2022_PROGRAM_ID);
      feesWithheld += (amount * 300n) / 10_000n;
    }
  });

  // ---------------- setup guards ----------------

  it("only the upgrade authority can initialize (no front-running)", async () => {
    await expectErr(
      program.methods.initialize(initArgs()).accountsPartial(initAccounts(attacker.publicKey, token)).signers([attacker]).rpc(),
      "Unauthorized"
    );
  });

  it("refuses a token whose fee authority is not the program", async () => {
    const bad = await createFeeMint(connection, admin, { decimals: DEC, supplyWhole: 1_000n, feeBps: 300, programId: Keypair.generate().publicKey });
    await expectErr(program.methods.initialize(initArgs()).accountsPartial(initAccounts(admin.publicKey, bad)).rpc(), "MintFeeAuthorityMismatch");
  });

  it("refuses a token with a permanent delegate", async () => {
    const bad = await createFeeMint(connection, admin, {
      decimals: DEC, supplyWhole: 1_000n, feeBps: 300, programId: program.programId, permanentDelegate: attacker.publicKey,
    });
    await expectErr(program.methods.initialize(initArgs()).accountsPartial(initAccounts(admin.publicKey, bad)).rpc(), "MintExtensionNotAllowed");
  });

  it("refuses a token with a freeze authority", async () => {
    const bad = await createFeeMint(connection, admin, {
      decimals: DEC, supplyWhole: 1_000n, feeBps: 300, programId: program.programId, freezeAuthority: attacker.publicKey,
    });
    await expectErr(program.methods.initialize(initArgs()).accountsPartial(initAccounts(admin.publicKey, bad)).rpc(), "FreezeAuthorityPresent");
  });

  it("initializes and creates the vaults", async () => {
    await program.methods.initialize(initArgs()).accountsPartial(initAccounts(admin.publicKey, token)).rpc();
    await program.methods
      .initVaultsA()
      .accountsPartial({
        admin: admin.publicKey, config, tokenMint: token, usdcMint: usdc, feeVault, reserveVault, discountVault,
        tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID, systemProgram: SystemProgram.programId,
      })
      .rpc();
    await program.methods
      .initVaultsB()
      .accountsPartial({
        admin: admin.publicKey, config, usdcMint: usdc, opsVault, pendingVault, bonusVault,
        tokenProgram: TOKEN_PROGRAM_ID, systemProgram: SystemProgram.programId,
      })
      .rpc();
    const c = await cfg();
    assert.equal(c.tokenMint.toBase58(), token.toBase58());
    assert.equal(c.roles.operatorTreasury.toBase58(), treasury.toBase58());
  });

  // ---------------- fees and the auction ----------------

  const harvestAndCollect = async (sources: PublicKey[]) => {
    const tx = new Transaction().add(
      createHarvestWithheldTokensToMintInstruction(token, sources, TOKEN_2022_PROGRAM_ID),
      await program.methods.collectFees().accountsPartial({ config, tokenMint: token, feeVault, token2022Program: TOKEN_2022_PROGRAM_ID }).instruction()
    );
    await provider.sendAndConfirm(tx);
  };
  const buyFeesIx = (amount: bigint, max: BN) =>
    program.methods
      .buyFees(new BN(amount.toString()), max)
      .accountsPartial({
        buyer: buyer.publicKey, config, tokenMint: token, usdcMint: usdc, feeVault, reserveVault, discountVault, opsVault,
        buyerUsdc: ataUsdc(buyer.publicKey), buyerToken: ataTok(buyer.publicKey),
        tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID,
      });

  it("collects the withheld tax and burns its share", async () => {
    await harvestAndCollect([ataTok(alice.publicKey), ataTok(bob.publicKey)]);
    const burned = (feesWithheld * 667n) / 10_000n;
    assert.equal(await bal(feeVault, TOKEN_2022_PROGRAM_ID), feesWithheld - burned);
  });

  it("sells the fee lot by Dutch auction, splits the USDC, and does not reset the clock", async () => {
    const before = await cfg();
    const lot = await bal(feeVault, TOKEN_2022_PROGRAM_ID);
    const usdcBefore = await bal(ataUsdc(buyer.publicKey));
    await buyFeesIx(lot, usd(10_000)).signers([buyer]).rpc();
    const paid = usdcBefore - (await bal(ataUsdc(buyer.publicKey)));
    const r = await bal(reserveVault);
    const d = await bal(discountVault);
    const o = await bal(opsVault);
    assert.equal(r + d + o, paid);
    assert.equal(d, (paid * 1071n) / 10_000n);
    assert.equal(o, (paid * 1429n) / 10_000n);
    const after = await cfg();
    assert.equal(BigInt(after.totalReserveIn.toString()), r);
    assert.equal(after.auction.startPrice.toString(), before.auction.startPrice.toString(), "a sale must not move the start price");
    assert.equal(after.auction.startedAt.toString(), before.auction.startedAt.toString(), "a sale must not reset the clock");
    assert.isTrue(after.auction.lastClearPrice.gtn(0));
  });

  it("refuses to restart the auction while there is nothing to sell", async () => {
    await expectErr(program.methods.restartAuction().accountsPartial({ config, feeVault }).rpc(), "FeeVaultEmpty");
  });

  it("dust buys inside one transaction cannot pump the price", async () => {
    // New taxed transfer -> new lot.
    await transferChecked(connection, alice, ataTok(alice.publicKey), token, ataTok(bob.publicKey), alice, 1_000_000n * ONE, DEC, [], undefined, TOKEN_2022_PROGRAM_ID);
    await harvestAndCollect([ataTok(bob.publicKey)]);
    const before = await cfg();
    const tx = new Transaction();
    for (let i = 0; i < 8; i++) tx.add(await buyFeesIx(1n, usd(1)).instruction());
    await provider.sendAndConfirm(tx, [buyer]);
    const after = await cfg();
    assert.equal(after.auction.startPrice.toString(), before.auction.startPrice.toString());
    assert.equal(after.auction.startedAt.toString(), before.auction.startedAt.toString());
    // A big new batch on top of the leftover starts a fresh auction instead of joining the decayed one,
    // and the growth cap keeps its start within one markup of the previous start.
    await sleep(1500);
    await transferChecked(connection, alice, ataTok(alice.publicKey), token, ataTok(bob.publicKey), alice, 2_000_000n * ONE, DEC, [], undefined, TOKEN_2022_PROGRAM_ID);
    await harvestAndCollect([ataTok(bob.publicKey)]);
    const fresh = await cfg();
    assert.notEqual(fresh.auction.startedAt.toString(), after.auction.startedAt.toString(), "a big batch should start a new auction");
    const prev = BigInt(after.auction.startPrice.toString());
    assert.isTrue(BigInt(fresh.auction.startPrice.toString()) <= (prev * 12_000n) / 10_000n + 1n);
    // Clear the rest of the lot so later reserve maths is simple.
    const rest = await bal(feeVault, TOKEN_2022_PROGRAM_ID);
    await buyFeesIx(rest, usd(10_000)).signers([buyer]).rpc();
  });

  // ---------------- credit roots ----------------

  let tree: CreditTree;
  const postRoot = (t: CreditTree, epoch: number, total?: bigint) =>
    program.methods
      .postRoot(Array.from(t.root()), new BN(epoch), new BN((total ?? t.total()).toString()))
      .accountsPartial({ indexer: indexer.publicKey, config })
      .signers([indexer])
      .rpc();
  const activateRoot = () => program.methods.activateRoot().accountsPartial({ config }).rpc();

  it("refuses a root that would promise more credit than backing", async () => {
    const reserveIn = BigInt((await cfg()).totalReserveIn.toString());
    const over = new CreditTree([
      { wallet: alice.publicKey, cumulative: reserveIn },
      { wallet: bob.publicKey, cumulative: 1n },
    ]);
    await expectErr(postRoot(over, 1), "Insolvent");
  });

  it("a new root waits out its delay before it can be used", async () => {
    tree = new CreditTree([
      { wallet: alice.publicKey, cumulative: 200n * ONE },
      { wallet: bob.publicKey, cumulative: 50n * ONE },
    ]);
    assert.isTrue(tree.total() <= BigInt((await cfg()).totalReserveIn.toString()));
    await postRoot(tree, 1);
    await expectErr(activateRoot(), "RootNotDue");
    await sleep((ROOT_DELAY + 1) * 1000);
    await activateRoot();
    assert.equal((await cfg()).activeRoot.epoch.toString(), "1");
  });

  it("epochs must go up by exactly one", async () => {
    await expectErr(postRoot(tree, 9), "StaleEpoch");
  });

  it("the admin can veto a pending root", async () => {
    await postRoot(tree, 2);
    await program.methods.vetoRoot().accountsPartial({ admin: admin.publicKey, config }).rpc();
    const c = await cfg();
    assert.isFalse(c.hasPendingRoot);
    assert.equal(c.activeRoot.epoch.toString(), "1");
  });

  // ---------------- redemptions ----------------

  const redeemAccounts = (u: Keypair, id: number) => ({
    user: u.publicKey, config, usdcMint: usdc, userState: userState(u.publicKey), redemption: redemption(u.publicKey, id),
    reserveVault, pendingVault, tokenProgram: TOKEN_PROGRAM_ID, systemProgram: SystemProgram.programId,
  });
  const redeemArgs = (t: CreditTree, u: Keypair, id: number, amount: BN) => ({
    requestId: new BN(id), amount, cumulative: new BN(t.cumulativeOf(u.publicKey).toString()),
    proof: t.proof(u.publicKey).map((p) => Array.from(p)), item: 1, platform: 0, region: US,
  });
  const redeem = (t: CreditTree, u: Keypair, id: number, amount: BN) =>
    program.methods.requestRedemption(redeemArgs(t, u, id, amount)).accountsPartial(redeemAccounts(u, id)).signers([u]).rpc();
  const fulfill = (u: PublicKey, id: number) =>
    program.methods
      .fulfillRedemption(Array.from(Buffer.alloc(32, 7)))
      .accountsPartial({
        operator: operator.publicKey, config, usdcMint: usdc, redemption: redemption(u, id), pendingVault,
        operatorTreasury: treasury, tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([operator])
      .rpc();
  const cancelAccounts = (u: PublicKey, id: number, refund: PublicKey | null) => ({
    config, usdcMint: usdc, redemption: redemption(u, id), userState: userState(u), pendingVault, reserveVault, discountVault,
    // Optional account: only purchases need the user's USDC account for the refund.
    userUsdc: (refund ?? null) as unknown as PublicKey,
    tokenProgram: TOKEN_PROGRAM_ID,
  });

  it("alice spends vested credit", async () => {
    await redeem(tree, alice, 0, usd(20));
    assert.equal(await bal(pendingVault), 20n * ONE);
    const us = await acct.userState.fetch(userState(alice.publicKey));
    assert.equal(us.consumed.toString(), (20n * ONE).toString());
  });

  it("stops overspending and forged proofs", async () => {
    await expectErr(redeem(tree, alice, 1, usd(190)), "InsufficientCredit");
    const forged = { ...redeemArgs(tree, alice, 1, usd(20)), cumulative: usd(250) };
    await expectErr(program.methods.requestRedemption(forged).accountsPartial(redeemAccounts(alice, 1)).signers([alice]).rpc(), "InvalidProof");
  });

  it("operator fulfils with a receipt hash", async () => {
    await fulfill(alice.publicKey, 0);
    assert.equal(await bal(treasury), 20n * ONE);
    assert.equal((await acct.redemption.fetch(redemption(alice.publicKey, 0))).status, 1);
  });

  it("the user can close a settled request and get its rent back", async () => {
    const before = await connection.getBalance(alice.publicKey, commitment);
    await program.methods.closeRedemption().accountsPartial({ user: alice.publicKey, redemption: redemption(alice.publicKey, 0) }).signers([alice]).rpc();
    assert.isNull(await connection.getAccountInfo(redemption(alice.publicKey, 0), commitment));
    assert.isAbove(await connection.getBalance(alice.publicKey, commitment), before);
  });

  it("operator cancel returns USDC to backing and restores credit", async () => {
    const reserveBefore = await bal(reserveVault);
    await redeem(tree, alice, 1, usd(15));
    await program.methods.cancelRedemption().accountsPartial({ authority: operator.publicKey, ...cancelAccounts(alice.publicKey, 1, null) }).signers([operator]).rpc();
    assert.equal(await bal(reserveVault), reserveBefore);
    const us = await acct.userState.fetch(userState(alice.publicKey));
    assert.equal(us.consumed.toString(), (20n * ONE).toString());
  });

  it("a user cannot cancel before the request's own cancel time", async () => {
    await redeem(tree, alice, 2, usd(15));
    await expectErr(
      program.methods.cancelRedemption().accountsPartial({ authority: alice.publicKey, ...cancelAccounts(alice.publicKey, 2, null) }).signers([alice]).rpc(),
      "CancelTooEarly"
    );
  });

  it("bob buys GTA 6 credit at face and gets the 10% member rebate", async () => {
    const before = await bal(ataUsdc(bob.publicKey));
    const poolBefore = await bal(discountVault);
    await program.methods
      .buyCredit({ requestId: new BN(0), price: usd(20), item: 1, platform: 1, region: US })
      .accountsPartial({
        user: bob.publicKey, config, usdcMint: usdc, tokenMint: token, userState: userState(bob.publicKey), redemption: redemption(bob.publicKey, 0),
        userUsdc: ataUsdc(bob.publicKey), userToken: ataTok(bob.publicKey), discountVault, pendingVault,
        tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID, systemProgram: SystemProgram.programId,
      })
      .signers([bob])
      .rpc();
    assert.equal(before - (await bal(ataUsdc(bob.publicKey))), 18n * ONE);
    assert.equal(poolBefore - (await bal(discountVault)), 2n * ONE);
    const r = await acct.redemption.fetch(redemption(bob.publicKey, 0));
    assert.equal(r.covered.toString(), (20n * ONE).toString());
  });

  it("membership must come from the wallet's own associated token account", async () => {
    const loose = await createAccount(connection, admin, token, bob.publicKey, Keypair.generate(), undefined, TOKEN_2022_PROGRAM_ID);
    await expectErr(
      program.methods
        .buyCredit({ requestId: new BN(1), price: usd(20), item: 1, platform: 1, region: US })
        .accountsPartial({
          user: bob.publicKey, config, usdcMint: usdc, tokenMint: token, userState: userState(bob.publicKey), redemption: redemption(bob.publicKey, 1),
          userUsdc: ataUsdc(bob.publicKey), userToken: loose, discountVault, pendingVault,
          tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID, systemProgram: SystemProgram.programId,
        })
        .signers([bob])
        .rpc(),
      "ConstraintAssociated"
    );
  });

  it("settlement is capped per day, and a processing request can still be cancelled by the operator", async () => {
    await fulfill(bob.publicKey, 0); // 20 + 20 = 40 of the 50/day cap
    await program.methods
      .markProcessing()
      .accountsPartial({ operator: operator.publicKey, config, redemption: redemption(alice.publicKey, 2) })
      .signers([operator])
      .rpc();
    assert.equal((await acct.redemption.fetch(redemption(alice.publicKey, 2))).status, 3);
    await expectErr(fulfill(alice.publicKey, 2), "FulfillCapExceeded"); // 40 + 15 > 50
    await expectErr(
      program.methods.cancelRedemption().accountsPartial({ authority: alice.publicKey, ...cancelAccounts(alice.publicKey, 2, null) }).signers([alice]).rpc(),
      "CancelTooEarly"
    );
    await program.methods.cancelRedemption().accountsPartial({ authority: operator.publicKey, ...cancelAccounts(alice.publicKey, 2, null) }).signers([operator]).rpc();
    assert.equal((await acct.redemption.fetch(redemption(alice.publicKey, 2))).status, 2);
  });

  it("total redemptions can never exceed the vested total, even if the leaves over-promise", async () => {
    // A buggy tree: leaves add up to 450, but the posted total is 260.
    const buggy = new CreditTree([
      { wallet: alice.publicKey, cumulative: 400n * ONE },
      { wallet: bob.publicKey, cumulative: 50n * ONE },
    ]);
    await postRoot(buggy, 2, 260n * ONE);
    await sleep((ROOT_DELAY + 1) * 1000);
    await activateRoot();
    // consumed is 20; 20 + 250 = 270 > 260.
    await expectErr(redeem(buggy, alice, 3, usd(250)), "Overissued");
  });

  // ---------------- admin powers ----------------

  it("role and parameter changes wait out the timelock", async () => {
    await program.methods
      .proposeConfig({ roles: { indexer: attacker.publicKey, operator: attacker.publicKey, operatorTreasury: treasury }, params, reanchorPrice: new BN(0) })
      .accountsPartial({ admin: admin.publicKey, config })
      .rpc();
    await expectErr(
      program.methods.applyConfig().accountsPartial({ admin: admin.publicKey, config, usdcMint: usdc, operatorTreasury: treasury, tokenProgram: TOKEN_PROGRAM_ID }).rpc(),
      "TimelockActive"
    );
    await program.methods.cancelConfig().accountsPartial({ admin: admin.publicKey, config }).rpc();
    assert.equal((await cfg()).roles.operator.toBase58(), operator.publicKey.toBase58());
  });

  it("the transfer fee can only strictly go down", async () => {
    const setFee = (bps: number, max: BN) =>
      program.methods.setTransferFee(bps, max).accountsPartial({ admin: admin.publicKey, config, tokenMint: token, token2022Program: TOKEN_2022_PROGRAM_ID }).rpc();
    await expectErr(setFee(400, U64_MAX), "FeeCanOnlyDecrease");
    await expectErr(setFee(300, U64_MAX), "FeeCanOnlyDecrease");
    await setFee(200, U64_MAX);
  });

  it("pausing blocks new redemptions and expires on its own", async () => {
    await program.methods.setPaused(true).accountsPartial({ admin: admin.publicKey, config }).rpc();
    const c = await cfg();
    assert.isTrue(c.pausedUntil.gtn(0));
    await expectErr(redeem(tree, alice, 3, usd(10)), "Paused");
    await program.methods.setPaused(false).accountsPartial({ admin: admin.publicKey, config }).rpc();
  });

  it("admin handover takes two steps", async () => {
    await program.methods.transferAdmin(newAdmin.publicKey).accountsPartial({ admin: admin.publicKey, config }).rpc();
    await expectErr(program.methods.acceptAdmin().accountsPartial({ newAdmin: attacker.publicKey, config }).signers([attacker]).rpc(), "NotPendingAdmin");
    await program.methods.acceptAdmin().accountsPartial({ newAdmin: newAdmin.publicKey, config }).signers([newAdmin]).rpc();
    assert.equal((await cfg()).admin.toBase58(), newAdmin.publicKey.toBase58());
    // Hand it back.
    await program.methods.transferAdmin(admin.publicKey).accountsPartial({ admin: newAdmin.publicKey, config }).signers([newAdmin]).rpc();
    await program.methods.acceptAdmin().accountsPartial({ newAdmin: admin.publicKey, config }).rpc();
  });
});
