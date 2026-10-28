// Bounded framing, strict JSON, canonical material and protobuf scanning for
// the private native-model SDK child.
//
// Every helper refuses before allocating past the caller's bound. The strict
// JSON parser is a real recursive-descent parser: `JSON.parse` silently keeps
// the last duplicate object member, but duplicate/escaped object keys,
// non-integer numbers, NUL bytes, trailing content and depth above 16 are
// refused here.

import { createHash } from 'node:crypto';
import * as zlib from 'node:zlib';

export const MAX_DEPTH = 16;

/** Read exactly one newline-terminated document from stdin, bounded. */
export const readOneLine = (stream, limit) =>
  new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    let settled = false;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      stream.removeListener('data', onData);
      stream.removeListener('end', onEnd);
      stream.removeListener('error', onError);
      if (error) reject(error);
      else resolve(value);
    };
    const onData = chunk => {
      size += chunk.length;
      if (size > limit + 1) {
        finish(new Error('bounded input exceeded'));
        return;
      }
      chunks.push(chunk);
      const buffer = Buffer.concat(chunks, size);
      const newline = buffer.indexOf(0x0a);
      if (newline >= 0) {
        if (newline !== buffer.length - 1) {
          finish(new Error('trailing bytes after frame'));
          return;
        }
        finish(null, buffer.subarray(0, newline));
      }
    };
    const onEnd = () => finish(new Error('bounded input closed without a newline'));
    const onError = () => finish(new Error('bounded input failed'));
    stream.on('data', onData);
    stream.on('end', onEnd);
    stream.on('error', onError);
  });

class StrictParser {
  constructor(text) {
    this.text = text;
    this.index = 0;
  }

  fail() {
    throw new Error('json is malformed');
  }

  skipWhitespace() {
    while (this.index < this.text.length) {
      const ch = this.text[this.index];
      if (ch === ' ' || ch === '\t' || ch === '\n' || ch === '\r') this.index += 1;
      else break;
    }
  }

  parseDocument() {
    this.skipWhitespace();
    const value = this.parseValue(0);
    this.skipWhitespace();
    if (this.index !== this.text.length) this.fail();
    return value;
  }

  parseValue(depth) {
    if (depth > MAX_DEPTH) throw new Error('json is too deep');
    this.skipWhitespace();
    if (this.index >= this.text.length) this.fail();
    const ch = this.text[this.index];
    if (ch === '{') return this.parseObject(depth);
    if (ch === '[') return this.parseArray(depth);
    if (ch === '"') return this.parseString(false);
    if (ch === 't') return this.parseLiteral('true', true);
    if (ch === 'f') return this.parseLiteral('false', false);
    if (ch === 'n') return this.parseLiteral('null', null);
    return this.parseNumber();
  }

  parseLiteral(text, value) {
    if (this.text.slice(this.index, this.index + text.length) !== text) this.fail();
    this.index += text.length;
    return value;
  }

  parseNumber() {
    const start = this.index;
    if (this.text[this.index] === '-') this.index += 1;
    const digitsStart = this.index;
    while (this.index < this.text.length && this.text.charCodeAt(this.index) >= 0x30 && this.text.charCodeAt(this.index) <= 0x39) {
      this.index += 1;
    }
    if (this.index === digitsStart) this.fail();
    // Reject a leading zero and any fractional/exponent form: this transport
    // carries integers only.
    const digits = this.text.slice(digitsStart, this.index);
    if (digits.length > 1 && digits[0] === '0') this.fail();
    if (this.index < this.text.length && (this.text[this.index] === '.' || this.text[this.index] === 'e' || this.text[this.index] === 'E')) {
      throw new Error('non-integer json number');
    }
    const value = Number(this.text.slice(start, this.index));
    if (!Number.isSafeInteger(value)) throw new Error('non-integer json number');
    return value;
  }

  parseString(isKey) {
    if (this.text[this.index] !== '"') this.fail();
    this.index += 1;
    let value = '';
    while (this.index < this.text.length) {
      const ch = this.text[this.index];
      if (ch === '"') {
        this.index += 1;
        return value;
      }
      if (ch === '\\') {
        if (isKey) throw new Error('escaped object key is not canonical');
        const escape = this.text[this.index + 1];
        this.index += 2;
        if (escape === '"') value += '"';
        else if (escape === '\\') value += '\\';
        else if (escape === '/') value += '/';
        else if (escape === 'b') value += '\b';
        else if (escape === 'f') value += '\f';
        else if (escape === 'n') value += '\n';
        else if (escape === 'r') value += '\r';
        else if (escape === 't') value += '\t';
        else if (escape === 'u') {
          const hex = this.text.slice(this.index, this.index + 4);
          if (!/^[0-9a-fA-F]{4}$/.test(hex)) this.fail();
          value += String.fromCharCode(Number.parseInt(hex, 16));
          this.index += 4;
        } else this.fail();
        continue;
      }
      const code = this.text.charCodeAt(this.index);
      if (code < 0x20) this.fail();
      value += ch;
      this.index += 1;
    }
    this.fail();
  }

  parseObject(depth) {
    this.index += 1;
    const result = Object.create(null);
    this.skipWhitespace();
    if (this.text[this.index] === '}') {
      this.index += 1;
      return result;
    }
    while (true) {
      this.skipWhitespace();
      const key = this.parseString(true);
      if (Object.prototype.hasOwnProperty.call(result, key)) throw new Error('duplicate object key');
      this.skipWhitespace();
      if (this.text[this.index] !== ':') this.fail();
      this.index += 1;
      result[key] = this.parseValue(depth + 1);
      this.skipWhitespace();
      const ch = this.text[this.index];
      if (ch === ',') {
        this.index += 1;
        continue;
      }
      if (ch === '}') {
        this.index += 1;
        return result;
      }
      this.fail();
    }
  }

  parseArray(depth) {
    this.index += 1;
    const result = [];
    this.skipWhitespace();
    if (this.text[this.index] === ']') {
      this.index += 1;
      return result;
    }
    while (true) {
      result.push(this.parseValue(depth + 1));
      this.skipWhitespace();
      const ch = this.text[this.index];
      if (ch === ',') {
        this.index += 1;
        continue;
      }
      if (ch === ']') {
        this.index += 1;
        return result;
      }
      this.fail();
    }
  }
}

/** Strict JSON parse of one bounded UTF-8 document. */
export const parseStrictJson = (buffer, limit) => {
  if (!Buffer.isBuffer(buffer)) throw new Error('json is not bytes');
  if (buffer.length === 0 || buffer.length > limit) throw new Error('json is out of bounds');
  const text = buffer.toString('utf8');
  if (text.includes('\u0000')) throw new Error('json carries a NUL');
  return new StrictParser(text).parseDocument();
};

/** Sorted-key, no-whitespace JSON with integer numbers only. */
export const canonical = value => {
  if (value === null) return 'null';
  if (value === true) return 'true';
  if (value === false) return 'false';
  if (typeof value === 'number') {
    if (!Number.isSafeInteger(value)) throw new Error('non-integer json value');
    return String(value);
  }
  if (typeof value === 'string') return JSON.stringify(value);
  if (Array.isArray(value)) return '[' + value.map(canonical).join(',') + ']';
  if (typeof value === 'object') {
    const keys = Object.keys(value).sort();
    return '{' + keys.map(key => JSON.stringify(key) + ':' + canonical(value[key])).join(',') + '}';
  }
  throw new Error('unsupported json value');
};

export const sha256 = value => 'sha256:' + createHash('sha256').update(value).digest('hex');

/** Exact material digest: `sha256(domain + "\n" + canonical(value))`. */
export const materialDigest = (domain, value) =>
  sha256(Buffer.concat([Buffer.from(domain + '\n', 'utf8'), Buffer.from(canonical(value), 'utf8')]));

export const decodeVarint = (buffer, offset) => {
  let result = 0n;
  let shift = 0n;
  let index = offset;
  while (index < buffer.length) {
    const byte = buffer[index];
    index += 1;
    result |= BigInt(byte & 0x7f) << shift;
    if (!(byte & 0x80)) return [result, index];
    shift += 7n;
    if (shift > 63n) throw new Error('varint is out of bounds');
  }
  throw new Error('truncated varint');
};

/** Iterate protobuf fields with a hard field-count and nesting bound. */
export const iterFields = function* (buffer, depth = 0) {
  if (depth > 8) throw new Error('protobuf is too deep');
  let index = 0;
  let count = 0;
  while (index < buffer.length) {
    count += 1;
    if (count > 131072) throw new Error('protobuf field count is out of bounds');
    const [tagBig, afterTag] = decodeVarint(buffer, index);
    index = afterTag;
    const tag = Number(tagBig);
    const num = tag >> 3;
    const wire = tag & 0x7;
    if (wire === 0) {
      const [value, after] = decodeVarint(buffer, index);
      index = after;
      yield { num, wire, value };
    } else if (wire === 1) {
      if (index + 8 > buffer.length) throw new Error('truncated fixed64');
      yield { num, wire, value: buffer.subarray(index, index + 8) };
      index += 8;
    } else if (wire === 2) {
      const [lengthBig, afterLength] = decodeVarint(buffer, index);
      const length = Number(lengthBig);
      if (!Number.isSafeInteger(length) || length < 0 || afterLength + length > buffer.length) {
        throw new Error('truncated length-delimited field');
      }
      yield { num, wire, value: buffer.subarray(afterLength, afterLength + length) };
      index = afterLength + length;
    } else if (wire === 5) {
      if (index + 4 > buffer.length) throw new Error('truncated fixed32');
      yield { num, wire, value: buffer.subarray(index, index + 4) };
      index += 4;
    } else {
      throw new Error('unsupported protobuf wire type');
    }
  }
};

/** Bounded gunzip: refuses a bomb before expanding past `max`. */
export const boundedGunzip = (buffer, max) => {
  try {
    return zlib.gunzipSync(buffer, { maxOutputLength: max });
  } catch {
    throw new Error('compressed frame is not admissible');
  }
};

export const boundedGzip = (buffer, max) => {
  const output = zlib.gzipSync(buffer);
  if (output.length > max) throw new Error('compressed body is out of bounds');
  return output;
};
