/**
 * Credit tree builder. Must hash exactly like programs/credit_vault/src/math.rs:
 *   leaf = keccak256(0x00 || wallet(32) || cumulative_vested_u64_le)
 *   node = keccak256(0x01 || min(a,b) || max(a,b))
 * An odd node at any level is promoted unchanged.
 *
 * CLI: ts-node scripts/merkle.ts balances.json > tree.json
 *   balances.json = [{ "wallet": "<base58>", "cumulative": "12500000" }, ...]  (micro-USDC)
 */
import { keccak_256 } from "@noble/hashes/sha3";
import { PublicKey } from "@solana/web3.js";
import * as fs from "fs";

export type Entry = { wallet: PublicKey; cumulative: bigint };

const LEAF = Buffer.from([0x00]);
const NODE = Buffer.from([0x01]);

function u64le(n: bigint): Buffer {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(n);
  return b;
}

export function leafHash(wallet: PublicKey, cumulative: bigint): Buffer {
  return Buffer.from(keccak_256(Buffer.concat([LEAF, wallet.toBuffer(), u64le(cumulative)])));
}

export function nodeHash(a: Buffer, b: Buffer): Buffer {
  const [x, y] = Buffer.compare(a, b) <= 0 ? [a, b] : [b, a];
  return Buffer.from(keccak_256(Buffer.concat([NODE, x, y])));
}

export class CreditTree {
  readonly entries: Entry[];
  private readonly levels: Buffer[][];
  private readonly position = new Map<string, number>();

  constructor(entries: Entry[]) {
    if (entries.length === 0) throw new Error("empty tree");
    this.entries = entries;
    entries.forEach((e, i) => {
      const k = e.wallet.toBase58();
      if (this.position.has(k)) throw new Error(`duplicate wallet ${k}`);
      if (e.cumulative < 0n || e.cumulative > 0xffffffffffffffffn) throw new Error(`bad amount for ${k}`);
      this.position.set(k, i);
    });
    const levels: Buffer[][] = [entries.map((e) => leafHash(e.wallet, e.cumulative))];
    while (levels[levels.length - 1].length > 1) {
      const prev = levels[levels.length - 1];
      const next: Buffer[] = [];
      for (let i = 0; i < prev.length; i += 2) {
        next.push(i + 1 < prev.length ? nodeHash(prev[i], prev[i + 1]) : prev[i]);
      }
      levels.push(next);
    }
    this.levels = levels;
  }

  root(): Buffer {
    return this.levels[this.levels.length - 1][0];
  }

  /** Sum of all leaves; the program rejects a root whose total exceeds backing inflow. */
  total(): bigint {
    return this.entries.reduce((s, e) => s + e.cumulative, 0n);
  }

  proof(wallet: PublicKey): Buffer[] {
    let idx = this.position.get(wallet.toBase58());
    if (idx === undefined) throw new Error(`wallet not in tree: ${wallet.toBase58()}`);
    const proof: Buffer[] = [];
    for (let l = 0; l < this.levels.length - 1; l++) {
      const level = this.levels[l];
      const sib = idx ^ 1;
      if (sib < level.length) proof.push(level[sib]);
      idx = Math.floor(idx / 2);
    }
    return proof;
  }

  cumulativeOf(wallet: PublicKey): bigint {
    const idx = this.position.get(wallet.toBase58());
    if (idx === undefined) throw new Error(`wallet not in tree: ${wallet.toBase58()}`);
    return this.entries[idx].cumulative;
  }
}

if (require.main === module) {
  const file = process.argv[2];
  if (!file) {
    console.error("usage: ts-node scripts/merkle.ts balances.json");
    process.exit(1);
  }
  const raw: { wallet: string; cumulative: string }[] = JSON.parse(fs.readFileSync(file, "utf8"));
  const tree = new CreditTree(raw.map((r) => ({ wallet: new PublicKey(r.wallet), cumulative: BigInt(r.cumulative) })));
  const out = {
    root: tree.root().toString("hex"),
    total: tree.total().toString(),
    proofs: Object.fromEntries(
      tree.entries.map((e) => [
        e.wallet.toBase58(),
        { cumulative: e.cumulative.toString(), proof: tree.proof(e.wallet).map((p) => p.toString("hex")) },
      ])
    ),
  };
  process.stdout.write(JSON.stringify(out, null, 2) + "\n");
}
