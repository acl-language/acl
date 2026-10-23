// Minimal msgpack decoder for the ACL playground (self-contained, no CDN).
// Covers the subset ACL envelopes use: nil, bool, ints, strings, bin,
// arrays, maps. Returns plain JS objects; bin → Uint8Array.
function msgpackDecode(bytes) {
  let pos = 0;
  function inner() {
    if (pos >= bytes.length) throw new Error("unexpected end");
    const b = bytes[pos++];
    // positive fixint / negative fixint
    if (b <= 0x7f) return b;
    if (b >= 0xe0) return b - 0x100;
    // fixmap / fixarray / fixstr
    if (b >= 0x80 && b <= 0x8f) return mapN(b & 0x0f);
    if (b >= 0x90 && b <= 0x9f) return arrN(b & 0x0f);
    if (b >= 0xa0 && b <= 0xbf) return strN(b & 0x1f);
    switch (b) {
      case 0xc0: return null;
      case 0xc2: return false;
      case 0xc3: return true;
      case 0xc4: return binN(u8());
      case 0xc5: return binN(u16());
      case 0xc6: return binN(u32());
      case 0xca: { const f = new DataView(bytes.buffer, bytes.byteOffset + pos).getFloat32(0); pos += 4; return f; }
      case 0xcb: { const f = new DataView(bytes.buffer, bytes.byteOffset + pos).getFloat64(0); pos += 8; return f; }
      case 0xcc: return u8();
      case 0xcd: return u16();
      case 0xce: return u32();
      case 0xcf: return u64();
      case 0xd0: return i8();
      case 0xd1: return i16();
      case 0xd2: return i32();
      case 0xd3: return i64();
      case 0xd9: return strN(u8());
      case 0xda: return strN(u16());
      case 0xdb: return strN(u32());
      case 0xdc: return arrN(u16());
      case 0xdd: return arrN(u32());
      case 0xde: return mapN(u16());
      case 0xdf: return mapN(u32());
    }
    throw new Error("unsupported msgpack byte 0x" + b.toString(16));
  }
  function u8()  { return bytes[pos++]; }
  function u16() { return (bytes[pos++] << 8) | bytes[pos++]; }
  function u32() { return ((bytes[pos++] << 24) | (bytes[pos++] << 16) | (bytes[pos++] << 8) | bytes[pos++]) >>> 0; }
  function u64() { const hi = u32(), lo = u32(); return hi * 4294967296 + lo; }
  function i8()  { return (bytes[pos++] << 24) >> 24; }
  function i16() { return (bytes[pos++] << 24) >> 16 | bytes[pos++]; }
  function i32() { return (bytes[pos++] << 24) | (bytes[pos++] << 16) | (bytes[pos++] << 8) | bytes[pos++]; }
  function i64() { const hi = u32(), lo = u32(); return hi * 4294967296 + lo; }
  function strN(n) { const s = new TextDecoder().decode(bytes.subarray(pos, pos + n)); pos += n; return s; }
  function binN(n) { const a = bytes.slice(pos, pos + n); pos += n; return a; }
  function arrN(n) { const a = []; for (let i = 0; i < n; i++) a.push(inner()); return a; }
  function mapN(n) { const m = {}; for (let i = 0; i < n; i++) { const k = inner(); m[String(k)] = inner(); } return m; }
  return inner();
}
if (typeof module !== "undefined") module.exports = { msgpackDecode };
