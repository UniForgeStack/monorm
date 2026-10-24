/**
 * Decodes the packed rows of the native engine (layout: `native/src/pack.rs`; keep the two
 * in step) into the cell values of `NativeRows.values`: one decode per row set.
 */

import { Decimal } from "./decimal.js";
import type { NativeRows } from "./native.js";
import { MICROS, MICROS_MS } from "./date-micros.js";

// Wire tags shared with native/src/pack.rs.
const Tag = {
  NULL: 0,
  FALSE: 1,
  TRUE: 2,
  INT: 3,
  BIGINT: 4,
  FLOAT: 5,
  ASCII: 6,
  UTF8: 7,
  DATETIME: 8,
  DATETIME_MICROS: 9,
  DATE: 10,
  UUID: 11,
  JSON: 12,
  DECIMAL: 13,
  ARRAY: 14,
} as const;

/** The cells of `rows`, row after row; decoded on first use. */
export function cellsOf(rows: NativeRows): unknown[] {
  let values = rows.values;
  if (values === undefined) {
    values = decode(rows.packed, rows.n * rows.width);
    rows.values = values;
  }
  return values;
}

function decode(buf: Buffer, count: number): unknown[] {
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  const out: unknown[] = new Array(count);
  let pos = 0;

  const cell = (): unknown => {
    const tag = buf[pos++]!;
    switch (tag) {
      case Tag.NULL:
        return null;
      case Tag.FALSE:
        return false;
      case Tag.TRUE:
        return true;
      case Tag.INT: {
        const n = view.getInt32(pos, true);
        pos += 4;
        return n;
      }
      case Tag.BIGINT: {
        const n = view.getBigInt64(pos, true);
        pos += 8;
        return n;
      }
      case Tag.FLOAT: {
        const n = view.getFloat64(pos, true);
        pos += 8;
        return n;
      }
      case Tag.ASCII:
        return text(view.getUint32(pos, true), 4, "latin1");
      case Tag.UTF8:
        return text(view.getUint32(pos, true), 4, "utf8");
      case Tag.DATETIME_MICROS: {
        const ms = view.getFloat64(pos, true);
        const d = new Date(ms);
        Object.defineProperty(d, MICROS, { value: view.getUint16(pos + 8, true) });
        Object.defineProperty(d, MICROS_MS, { value: ms });
        pos += 10;
        return d;
      }
      case Tag.DATETIME:
      case Tag.DATE: {
        const d = new Date(view.getFloat64(pos, true));
        pos += 8;
        return d;
      }
      case Tag.UUID:
        return text(36, 0, "latin1");
      case Tag.JSON:
        return JSON.parse(text(view.getUint32(pos, true), 4, "utf8"));
      case Tag.DECIMAL:
        return new Decimal(text(view.getUint32(pos, true), 4, "latin1"));
      case Tag.ARRAY: {
        const items: unknown[] = new Array(view.getUint32(pos, true));
        pos += 4;
        for (let i = 0; i < items.length; i++) {
          items[i] = cell();
        }
        return items;
      }
      default:
        throw new Error(`corrupt row buffer: tag ${tag}`);
    }
  };
  const text = (length: number, header: number, encoding: BufferEncoding): string => {
    const start = pos + header;
    pos = start + length;
    return buf.toString(encoding, start, pos);
  };

  for (let i = 0; i < count; i++) {
    out[i] = cell();
  }
  return out;
}
