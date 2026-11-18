/**
 * Create the GTA token as a Token-2022 mint with a transfer fee, for the self-launch route
 * (mint here, then seed a Meteora DAMM v2 or Raydium CPMM pool, both of which support
 * transfer-fee tokens). Both fee authorities are set to the credit vault's config PDA, which
 * the program checks at `initialize`: only the program can withdraw the tax, and it can only
 * lower the rate.
 *
 * The full supply is minted to the payer's associated token account and the mint authority is
 * then revoked, so supply is fixed. No freeze authority is set.
 *
 * Usage:
 *   ts-node scripts/create-mint.ts --url http://127.0.0.1:8899 --keypair ~/.config/solana/id.json \
 *     --decimals 6 --supply 1000000000 --fee-bps 300
 */
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  AuthorityType,
  ExtensionType,
  TOKEN_2022_PROGRAM_ID,
  createAssociatedTokenAccountIdempotentInstruction,
  createInitializeMintInstruction,
  createInitializePermanentDelegateInstruction,
  createInitializeTransferFeeConfigInstruction,
  createMintToInstruction,
  createSetAuthorityInstruction,
  getAssociatedTokenAddressSync,
  getMintLen,
} from "@solana/spl-token";
import * as fs from "fs";
import * as os from "os";

export const PROGRAM_ID = new PublicKey("FxpPCZhoY2ecs9g9w5M9VK7m5fMW8mWF28hjhrE8QGtY");

export function configPda(programId: PublicKey = PROGRAM_ID): PublicKey {
  return PublicKey.findProgramAddressSync([Buffer.from("config")], programId)[0];
}

export async function createFeeMint(
  connection: Connection,
  payer: Keypair,
  opts: {
    decimals: number;
    supplyWhole: bigint;
    feeBps: number;
    programId?: PublicKey;
    mint?: Keypair;
    /** Test-only: build a deliberately unsafe mint that the program must refuse. */
    permanentDelegate?: PublicKey;
    freezeAuthority?: PublicKey;
  }
): Promise<PublicKey> {
  const mint = opts.mint ?? Keypair.generate();
  const authority = configPda(opts.programId ?? PROGRAM_ID);
  const extensions = [ExtensionType.TransferFeeConfig];
  if (opts.permanentDelegate) extensions.push(ExtensionType.PermanentDelegate);
  const mintLen = getMintLen(extensions);
  const lamports = await connection.getMinimumBalanceForRentExemption(mintLen);
  const maxFee = 0xffffffffffffffffn; // no per-transfer cap: the full percentage always applies
  const supply = opts.supplyWhole * 10n ** BigInt(opts.decimals);
  const ata = getAssociatedTokenAddressSync(mint.publicKey, payer.publicKey, false, TOKEN_2022_PROGRAM_ID);

  const tx = new Transaction().add(
    SystemProgram.createAccount({
      fromPubkey: payer.publicKey,
      newAccountPubkey: mint.publicKey,
      space: mintLen,
      lamports,
      programId: TOKEN_2022_PROGRAM_ID,
    })
  );
  if (opts.permanentDelegate) {
    tx.add(createInitializePermanentDelegateInstruction(mint.publicKey, opts.permanentDelegate, TOKEN_2022_PROGRAM_ID));
  }
  tx.add(
    createInitializeTransferFeeConfigInstruction(
      mint.publicKey,
      authority, // transfer fee config authority: can only be used through the program
      authority, // withdraw withheld authority: only the program can collect the tax
      opts.feeBps,
      maxFee,
      TOKEN_2022_PROGRAM_ID
    ),
    createInitializeMintInstruction(
      mint.publicKey,
      opts.decimals,
      payer.publicKey,
      opts.freezeAuthority ?? null,
      TOKEN_2022_PROGRAM_ID
    ),
    createAssociatedTokenAccountIdempotentInstruction(
      payer.publicKey,
      ata,
      payer.publicKey,
      mint.publicKey,
      TOKEN_2022_PROGRAM_ID
    ),
    createMintToInstruction(mint.publicKey, ata, payer.publicKey, supply, [], TOKEN_2022_PROGRAM_ID),
    createSetAuthorityInstruction(mint.publicKey, payer.publicKey, AuthorityType.MintTokens, null, [], TOKEN_2022_PROGRAM_ID)
  );
  await sendAndConfirmTransaction(connection, tx, [payer, mint]);
  return mint.publicKey;
}

function arg(name: string, fallback?: string): string {
  const i = process.argv.indexOf(`--${name}`);
  if (i >= 0 && process.argv[i + 1]) return process.argv[i + 1];
  if (fallback !== undefined) return fallback;
  throw new Error(`missing --${name}`);
}

if (require.main === module) {
  (async () => {
    const url = arg("url", "http://127.0.0.1:8899");
    const keypairPath = arg("keypair", `${os.homedir()}/.config/solana/id.json`).replace(/^~/, os.homedir());
    const payer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(keypairPath, "utf8"))));
    const connection = new Connection(url, "confirmed");
    const mint = await createFeeMint(connection, payer, {
      decimals: Number(arg("decimals", "6")),
      supplyWhole: BigInt(arg("supply", "1000000000")),
      feeBps: Number(arg("fee-bps", "300")),
    });
    console.log(JSON.stringify({ mint: mint.toBase58(), feeAuthority: configPda().toBase58() }, null, 2));
  })().catch((e) => {
    console.error(e);
    process.exit(1);
  });
}
