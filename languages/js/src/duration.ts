/**
 * `Duration`: a signed whole number of microseconds, for `Duration` columns. The value is
 * immutable. Its text is `[-][D ]HH:MM:SS[.ffffff]` (the Rust core, the admin and Python
 * use the same text); `Duration.parse` also reads a time-only ISO 8601 form (`PT1H30M`).
 */

const SECOND = 1_000_000n;
const MINUTE = 60n * SECOND;
const HOUR = 60n * MINUTE;
const DAY = 24n * HOUR;
const MS = 1000n;
const MIN = -(1n << 63n);
const MAX = (1n << 63n) - 1n;

export interface DurationParts {
  readonly days?: number;
  readonly hours?: number;
  readonly minutes?: number;
  readonly seconds?: number;
  readonly millis?: number;
  readonly micros?: number;
}

const wholeNumber = (n: number, what: string): bigint => {
  if (!Number.isSafeInteger(n)) throw new RangeError(`${what} must be a safe integer, got ${n}`);
  return BigInt(n);
};

export class Duration {
  /** The value in microseconds. */
  readonly micros: bigint;

  private constructor(micros: bigint) {
    if (micros < MIN || micros > MAX) throw new RangeError("a Duration is out of the 64-bit range of microseconds");
    this.micros = micros;
    Object.freeze(this);
  }

  static readonly ZERO = new Duration(0n);

  static fromMicros(micros: bigint | number): Duration {
    return new Duration(typeof micros === "bigint" ? micros : wholeNumber(micros, "micros"));
  }

  static fromMillis(millis: number): Duration {
    if (!Number.isFinite(millis)) throw new RangeError(`millis must be finite, got ${millis}`);
    return new Duration(BigInt(Math.round(millis * 1000)));
  }

  static fromParts(parts: DurationParts): Duration {
    const { days = 0, hours = 0, minutes = 0, seconds = 0, millis = 0, micros = 0 } = parts;
    return new Duration(
      wholeNumber(days, "days") * DAY +
        wholeNumber(hours, "hours") * HOUR +
        wholeNumber(minutes, "minutes") * MINUTE +
        wholeNumber(seconds, "seconds") * SECOND +
        wholeNumber(millis, "millis") * MS +
        wholeNumber(micros, "micros"),
    );
  }

  /** The text form, or the ISO 8601 time-only form (`PT1H30M`). */
  static parse(text: string): Duration {
    const bad = () => new RangeError(`"${text}" is not a duration: use [-][D ]HH:MM:SS[.ffffff] or PT1H30M`);
    const t = text.trim();
    const negative = t.startsWith("-");
    const body = negative || t.startsWith("+") ? t.slice(1) : t;
    let total: bigint | undefined;
    const iso = /^PT(?:(\d+)H)?(?:(\d+)M)?(?:(\d+)(?:\.(\d{1,6}))?S)?$/i.exec(body);
    if (iso && body.length > 2) {
      total = BigInt(iso[1] ?? 0) * HOUR + BigInt(iso[2] ?? 0) * MINUTE + BigInt(iso[3] ?? 0) * SECOND + BigInt((iso[4] ?? "").padEnd(6, "0") || 0);
    } else {
      const m = /^(?:(\d+) )?\s*(\d+):(\d{2}):(\d{2})(?:\.(\d{1,6}))?$/.exec(body);
      if (m && +m[3]! <= 59 && +m[4]! <= 59) {
        total =
          BigInt(m[1] ?? 0) * DAY + BigInt(m[2]!) * HOUR + BigInt(m[3]!) * MINUTE + BigInt(m[4]!) * SECOND + BigInt((m[5] ?? "").padEnd(6, "0") || 0);
      }
    }
    if (total === undefined) throw bad();
    const signed = negative ? -total : total;
    if (signed < MIN || signed > MAX) throw new RangeError(`"${text}" is out of range for a Duration`);
    return new Duration(signed);
  }

  /** Milliseconds as a number; the fraction below a millisecond stays in the number. */
  toMillis(): number {
    return Number(this.micros) / 1000;
  }

  add(other: Duration): Duration {
    return new Duration(this.micros + other.micros);
  }

  sub(other: Duration): Duration {
    return new Duration(this.micros - other.micros);
  }

  negate(): Duration {
    return new Duration(-this.micros);
  }

  equals(other: Duration): boolean {
    return this.micros === other.micros;
  }

  /** A negative number, zero or a positive number, as `Array.prototype.sort` wants. */
  compare(other: Duration): number {
    return this.micros < other.micros ? -1 : this.micros > other.micros ? 1 : 0;
  }

  toString(): string {
    const abs = this.micros < 0n ? -this.micros : this.micros;
    const days = abs / DAY;
    const rest = abs % DAY;
    const pad = (n: bigint, width = 2) => n.toString().padStart(width, "0");
    const frac = rest % SECOND;
    return (
      (this.micros < 0n ? "-" : "") +
      (days > 0n ? `${days} ` : "") +
      `${pad(rest / HOUR)}:${pad((rest % HOUR) / MINUTE)}:${pad((rest % MINUTE) / SECOND)}` +
      (frac > 0n ? `.${pad(frac, 6)}` : "")
    );
  }

  toJSON(): string {
    return this.toString();
  }
}
