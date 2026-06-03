/**
 * Word-level diff for the prompt-enhancer panel.
 *
 * Computes the longest common subsequence between two token streams,
 * then emits a series of insert / delete / equal spans that the UI can
 * render with color highlights. Zero dependencies — small enough to
 * keep in-repo per CLAUDE.md §2 supply-chain rules.
 */

export type DiffOp = "equal" | "insert" | "delete";

export interface DiffSegment {
  op: DiffOp;
  text: string;
}

/**
 * Tokenize a string into an alternating sequence of word and whitespace
 * tokens, preserving original spacing. This produces visually clean
 * diffs (insertions and deletions land on word boundaries instead of
 * mid-word).
 */
function tokenize(input: string): string[] {
  const out: string[] = [];
  if (!input) return out;
  // Split on word boundaries but keep the boundary characters as
  // separate tokens so we can reconstruct the original spacing.
  const re = /(\s+|[^\s\w]+|\w+)/g;
  let match: RegExpExecArray | null;
  while ((match = re.exec(input)) !== null) {
    if (match[0]) out.push(match[0]);
  }
  return out;
}

/**
 * Classic LCS table. O(n*m) memory which is fine for prompt-sized
 * inputs (hundreds, not millions, of tokens).
 */
function lcsTable(a: string[], b: string[]): number[][] {
  const n = a.length;
  const m = b.length;
  const table: number[][] = Array(n + 1)
    .fill(null)
    .map(() => Array(m + 1).fill(0));
  for (let i = 1; i <= n; i++) {
    for (let j = 1; j <= m; j++) {
      if (a[i - 1] === b[j - 1]) {
        table[i]![j] = (table[i - 1]?.[j - 1] ?? 0) + 1;
      } else {
        table[i]![j] = Math.max(table[i - 1]?.[j] ?? 0, table[i]?.[j - 1] ?? 0);
      }
    }
  }
  return table;
}

/**
 * Walk the LCS table backwards and emit diff segments in reverse, then
 * reverse the result. Adjacent same-op segments are merged so the UI
 * doesn't render dozens of single-token spans.
 */
export function diffWords(original: string, enhanced: string): DiffSegment[] {
  const a = tokenize(original);
  const b = tokenize(enhanced);
  if (a.length === 0 && b.length === 0) return [];
  if (a.length === 0) return [{ op: "insert", text: enhanced }];
  if (b.length === 0) return [{ op: "delete", text: original }];

  const table = lcsTable(a, b);
  const segs: DiffSegment[] = [];
  let i = a.length;
  let j = b.length;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && a[i - 1] === b[j - 1]) {
      segs.push({ op: "equal", text: a[i - 1]! });
      i--;
      j--;
    } else if (j > 0 && (i === 0 || (table[i]?.[j - 1] ?? 0) >= (table[i - 1]?.[j] ?? 0))) {
      segs.push({ op: "insert", text: b[j - 1]! });
      j--;
    } else if (i > 0) {
      segs.push({ op: "delete", text: a[i - 1]! });
      i--;
    } else {
      break;
    }
  }
  segs.reverse();
  return mergeAdjacent(segs);
}

function mergeAdjacent(segs: DiffSegment[]): DiffSegment[] {
  if (segs.length <= 1) return segs;
  const out: DiffSegment[] = [];
  for (const seg of segs) {
    const last = out[out.length - 1];
    if (last && last.op === seg.op) {
      last.text += seg.text;
    } else {
      out.push({ ...seg });
    }
  }
  return out;
}

/**
 * Convenience: count how many words were inserted vs deleted. The
 * panel uses this to show a tiny "+12 -3" badge.
 */
export function diffStats(segments: DiffSegment[]): { inserted: number; deleted: number } {
  let inserted = 0;
  let deleted = 0;
  for (const seg of segments) {
    if (seg.op === "equal") continue;
    const wordCount = seg.text
      .split(/\s+/)
      .filter(token => /\w/.test(token)).length;
    if (seg.op === "insert") inserted += wordCount;
    else deleted += wordCount;
  }
  return { inserted, deleted };
}
