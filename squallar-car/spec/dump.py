#!/usr/bin/env python3
"""carspec-dump: independent decoder / checker for Apple asset catalogs (Assets.car).

Standalone: Python 3 stdlib only; PIL is optional (only for --pixels-dir PNG output).

    carspec-dump.py Assets.car                      full dump with byte offsets
    carspec-dump.py Assets.car --check-against assetutil.json
                                                    assert the decode agrees with Apple's
                                                    `xcrun assetutil --info` output
    carspec-dump.py Assets.car --strict-actool      also assert the layout conventions
                                                    observed on the Xcode 26.6 goldens
    carspec-dump.py Assets.car --pixels-dir DIR     decode every image rendition (LZFSE)
                                                    and write DIR/<name>.png + .bgra

Every error message names the absolute byte offset it was raised at.
Exit status: 0 = decoded (and all requested checks passed), 1 = problem.

Structure decoded (see carspec-SPEC.md for the byte-level layout):
  BOM container: header, block table, free list, vars
  CARHEADER, EXTENDED_METADATA, KEYFORMAT
  APPEARANCEKEYS, FACETKEYS, BITMAPKEYS, RENDITIONS trees
  each rendition: key, CSI header, TLVs, payload (CELM v3 chunks / MSIS / RAWD ...)
"""
import argparse
import hashlib
import json
import os
import struct
import sys

# ============================================================ LZFSE decoder
# (verbatim copy of the decoder validated against Apple's liblzfse on every golden chunk)
# ---------------------------------------------------------------- constants
L_SYMBOLS, M_SYMBOLS, D_SYMBOLS, LIT_SYMBOLS = 20, 20, 64, 256
L_STATES, M_STATES, D_STATES, LIT_STATES = 64, 64, 256, 1024
MATCHES_PER_BLOCK = 10000
LITERALS_PER_BLOCK = 4 * MATCHES_PER_BLOCK

L_EXTRA_BITS = [0]*16 + [2, 3, 5, 8]
L_BASE = list(range(16)) + [16, 20, 28, 60]
M_EXTRA_BITS = [0]*16 + [3, 5, 8, 11]
M_BASE = list(range(16)) + [16, 24, 56, 312]
D_EXTRA_BITS = [e for e in range(16) for _ in range(4)]
D_BASE = []
_b = 0
for _e in D_EXTRA_BITS:
    D_BASE.append(_b)
    _b += 1 << _e

FREQ_NBITS_TABLE = [2, 3, 2, 5, 2, 3, 2, 8, 2, 3, 2, 5, 2, 3, 2, 14,
                    2, 3, 2, 5, 2, 3, 2, 8, 2, 3, 2, 5, 2, 3, 2, 14]
FREQ_VALUE_TABLE = [0, 2, 1, 4, 0, 3, 1, -1, 0, 2, 1, 5, 0, 3, 1, -1,
                    0, 2, 1, 6, 0, 3, 1, -1, 0, 2, 1, 7, 0, 3, 1, -1]

MAGIC_ENDOFSTREAM = b'bvx$'
MAGIC_UNCOMPRESSED = b'bvx-'
MAGIC_COMPRESSED_V1 = b'bvx1'
MAGIC_COMPRESSED_V2 = b'bvx2'
MAGIC_COMPRESSED_LZVN = b'bvxn'


class LzfseError(Exception):
    pass


# ---------------------------------------------------------------- FSE core
def _clz32(x):
    return 32 - x.bit_length()


def fse_init_decoder_table(nstates, nsymbols, freq):
    """Return list of (k, symbol, delta) indexed by state (lzfse fse_init_decoder_table)."""
    n_clz = _clz32(nstates)
    table = []
    total = 0
    for i in range(nsymbols):
        f = freq[i]
        if f == 0:
            continue
        total += f
        if total > nstates:
            raise LzfseError("FSE frequency table sums to more than %d states" % nstates)
        k = _clz32(f) - n_clz
        j0 = ((2 * nstates) >> k) - f
        for j in range(f):
            if j < j0:
                table.append((k, i, ((f + j) << k) - nstates))
            else:
                table.append((k - 1, i, (j - j0) << (k - 1)))
    if len(table) != nstates:
        raise LzfseError("FSE frequency table sums to %d, expected %d states" % (len(table), nstates))
    return table


def fse_init_value_decoder_table(nstates, nsymbols, freq, vbits, vbase):
    """Return list of (total_bits, value_bits, delta, vbase) indexed by state."""
    n_clz = _clz32(nstates)
    table = []
    total = 0
    for i in range(nsymbols):
        f = freq[i]
        if f == 0:
            continue
        total += f
        if total > nstates:
            raise LzfseError("FSE value frequency table sums to more than %d states" % nstates)
        k = _clz32(f) - n_clz
        j0 = ((2 * nstates) >> k) - f
        for j in range(f):
            if j < j0:
                table.append((k + vbits[i], vbits[i], ((f + j) << k) - nstates, vbase[i]))
            else:
                table.append((k - 1 + vbits[i], vbits[i], (j - j0) << (k - 1), vbase[i]))
    if len(table) != nstates:
        raise LzfseError("FSE value frequency table sums to %d, expected %d states" % (len(table), nstates))
    return table


class BitReaderBackward:
    """lzfse fse_in_stream: reads the payload from its END backwards.

    The accumulator is refilled with the bytes just below the current pointer
    (little-endian); bits are pulled from the accumulator's top.  Equivalent
    to reading the payload's bytes from last to first, MSB-first within each
    byte, after skipping the (-n) padding bits of the last byte.

    Apple's decoder bounds the backward reads by the start of the WHOLE input
    buffer (s->src_begin), not by the start of the payload: the initial 8-byte
    load and later refills may pull bytes that precede the payload (the block
    header) into the accumulator's low bits; they are never consumed when the
    stream is well-formed.  We mimic that, and treat bytes before the buffer
    start as zero.
    """

    def __init__(self, buf, lower, end, nbits_init):
        self.buf = buf
        self.lower = lower
        self.pos = end
        self.accum = 0
        self.nbits = 0
        if nbits_init:
            self.pos -= 8
            self.accum = self._load(self.pos, 8)
            self.nbits = nbits_init + 64
        if self.nbits >= 64 or (self.accum >> self.nbits) != 0:
            raise LzfseError("FSE stream padding bits are not zero at end offset %d (nbits=%d)" % (end, self.nbits))

    def _load(self, p, n):
        if p >= self.lower:
            return int.from_bytes(self.buf[p:p + n], 'little')
        pad = self.lower - p
        if pad >= n:
            return 0
        return int.from_bytes(bytes(pad) + bytes(self.buf[self.lower:p + n]), 'little')

    def flush(self):
        nb = (63 - self.nbits) & ~7
        nbytes = nb >> 3
        if nbytes:
            p = self.pos - nbytes
            self.accum = (self.accum << nb) | self._load(p, nbytes)
            self.nbits += nb
            self.pos = p

    def pull(self, n):
        self.nbits -= n
        if self.nbits < 0:
            raise LzfseError("FSE stream ran out of bits at offset %d" % self.pos)
        r = self.accum >> self.nbits
        self.accum &= (1 << self.nbits) - 1
        return r


# ---------------------------------------------------------------- headers
def _get_field(v, off, nbits):
    return (v >> off) & ((1 << nbits) - 1)


def decode_v2_header(buf, off):
    """Decode a bvx2 header at buf[off:]; returns (header dict, header_size)."""
    if buf[off:off + 4] != MAGIC_COMPRESSED_V2:
        raise LzfseError("not a bvx2 block at %d" % off)
    n_raw_bytes = struct.unpack_from('<I', buf, off + 4)[0]
    v0, v1, v2 = struct.unpack_from('<QQQ', buf, off + 8)
    h = {}
    h['n_raw_bytes'] = n_raw_bytes
    h['n_literals'] = _get_field(v0, 0, 20)
    h['n_literal_payload_bytes'] = _get_field(v0, 20, 20)
    h['n_matches'] = _get_field(v0, 40, 20)
    h['literal_bits'] = _get_field(v0, 60, 3) - 7
    h['literal_state'] = [_get_field(v1, 0, 10), _get_field(v1, 10, 10), _get_field(v1, 20, 10), _get_field(v1, 30, 10)]
    h['n_lmd_payload_bytes'] = _get_field(v1, 40, 20)
    h['lmd_bits'] = _get_field(v1, 60, 3) - 7
    header_size = _get_field(v2, 0, 32)
    h['l_state'] = _get_field(v2, 32, 10)
    h['m_state'] = _get_field(v2, 42, 10)
    h['d_state'] = _get_field(v2, 52, 10)
    h['n_payload_bytes'] = h['n_literal_payload_bytes'] + h['n_lmd_payload_bytes']
    h['header_size'] = header_size
    # frequency tables, packed with the variable-length code
    nfreq = L_SYMBOLS + M_SYMBOLS + D_SYMBOLS + LIT_SYMBOLS
    freqs = []
    src = off + 32
    src_end = off + header_size
    accum = 0
    accum_nbits = 0
    for i in range(nfreq):
        while src < src_end and accum_nbits + 8 <= 32:
            accum |= buf[src] << accum_nbits
            accum_nbits += 8
            src += 1
        b = accum & 31
        n = FREQ_NBITS_TABLE[b]
        if n == 8:
            val = 8 + ((accum >> 4) & 0xf)
        elif n == 14:
            val = 24 + ((accum >> 4) & 0x3ff)
        else:
            val = FREQ_VALUE_TABLE[b]
        if n > accum_nbits:
            raise LzfseError("bvx2 header at %d: frequency table truncated" % off)
        accum >>= n
        accum_nbits -= n
        freqs.append(val)
    if accum_nbits >= 8 or src != src_end:
        raise LzfseError("bvx2 header at %d: frequency table size mismatch (header_size=%d)" % (off, header_size))
    h['l_freq'] = freqs[0:20]
    h['m_freq'] = freqs[20:40]
    h['d_freq'] = freqs[40:104]
    h['literal_freq'] = freqs[104:360]
    return h, header_size


def decode_v1_header(buf, off):
    if buf[off:off + 4] != MAGIC_COMPRESSED_V1:
        raise LzfseError("not a bvx1 block at %d" % off)
    f = struct.unpack_from('<IIIIIIIi4Hi3H', buf, off + 4)
    h = dict(n_raw_bytes=f[0], n_payload_bytes=f[1], n_literals=f[2], n_matches=f[3],
             n_literal_payload_bytes=f[4], n_lmd_payload_bytes=f[5], literal_bits=f[6],
             literal_state=list(f[7:11]), lmd_bits=f[11], l_state=f[12], m_state=f[13], d_state=f[14])
    p = off + 4 + 4 * 7 + 4 + 8 + 4 + 6
    freqs = list(struct.unpack_from('<%dH' % (L_SYMBOLS + M_SYMBOLS + D_SYMBOLS + LIT_SYMBOLS), buf, p))
    h['l_freq'] = freqs[0:20]
    h['m_freq'] = freqs[20:40]
    h['d_freq'] = freqs[40:104]
    h['literal_freq'] = freqs[104:360]
    h['header_size'] = 770
    return h, 770


# ---------------------------------------------------------------- block decoders
def decode_lzfse_block(buf, off, hdr, out):
    """Decode one LZFSE compressed block whose header (already parsed) starts at off.
    Appends decoded bytes to bytearray `out`. Returns offset after the block."""
    payload_start = off + hdr['header_size']
    lit_end = payload_start + hdr['n_literal_payload_bytes']
    lmd_end = lit_end + hdr['n_lmd_payload_bytes']
    if lmd_end > len(buf):
        raise LzfseError("LZFSE block at %d overruns the buffer (needs %d bytes, has %d)" % (off, lmd_end - off, len(buf) - off))
    lit_table = fse_init_decoder_table(LIT_STATES, LIT_SYMBOLS, hdr['literal_freq'])
    l_table = fse_init_value_decoder_table(L_STATES, L_SYMBOLS, hdr['l_freq'], L_EXTRA_BITS, L_BASE)
    m_table = fse_init_value_decoder_table(M_STATES, M_SYMBOLS, hdr['m_freq'], M_EXTRA_BITS, M_BASE)
    d_table = fse_init_value_decoder_table(D_STATES, D_SYMBOLS, hdr['d_freq'], D_EXTRA_BITS, D_BASE)

    # --- literals
    n_lit = hdr['n_literals']
    if n_lit > LITERALS_PER_BLOCK or n_lit % 4:
        raise LzfseError("LZFSE block at %d: n_literals=%d invalid" % (off, n_lit))
    rd = BitReaderBackward(buf, 0, lit_end, hdr['literal_bits'])
    states = list(hdr['literal_state'])
    literals = bytearray(n_lit)
    i = 0
    while i < n_lit:
        rd.flush()
        for s in range(4):
            k, sym, delta = lit_table[states[s]]
            states[s] = delta + rd.pull(k)
            literals[i + s] = sym
        i += 4

    # --- L, M, D
    rd = BitReaderBackward(buf, 0, lmd_end, hdr['lmd_bits'])
    ls, ms, ds = hdr['l_state'], hdr['m_state'], hdr['d_state']
    lit_pos = 0
    D = 0
    base = len(out)
    produced = 0
    for _ in range(hdr['n_matches']):
        rd.flush()
        tb, vb, delta, vbase = l_table[ls]
        v = rd.pull(tb)
        ls = delta + (v >> vb)
        L = vbase + (v & ((1 << vb) - 1))
        tb, vb, delta, vbase = m_table[ms]
        v = rd.pull(tb)
        ms = delta + (v >> vb)
        Mv = vbase + (v & ((1 << vb) - 1))
        tb, vb, delta, vbase = d_table[ds]
        v = rd.pull(tb)
        ds = delta + (v >> vb)
        newD = vbase + (v & ((1 << vb) - 1))
        if newD:
            D = newD
        if lit_pos + L > n_lit:
            raise LzfseError("LZFSE block at %d: match asks for %d literals, only %d left" % (off, L, n_lit - lit_pos))
        if L:
            out += literals[lit_pos:lit_pos + L]
            lit_pos += L
            produced += L
        if Mv:
            if D <= 0 or D > len(out):
                raise LzfseError("LZFSE block at %d: match distance %d exceeds output (%d)" % (off, D, len(out)))
            if Mv <= D:
                start = len(out) - D
                out += out[start:start + Mv]
            else:
                # overlapping copy: the pattern of length D repeats
                start = len(out) - D
                pat = out[start:]
                reps, rem = divmod(Mv, D)
                out += pat * reps + pat[:rem]
            produced += Mv
    if n_lit - lit_pos >= 4 or lit_pos > n_lit:
        # The encoder pads n_literals up to a multiple of 4; up to 3 trailing
        # padding literals are legitimate, anything more is a malformed block.
        raise LzfseError("LZFSE block at %d: %d literals decoded but only %d consumed by matches" % (off, n_lit, lit_pos))
    if produced != hdr['n_raw_bytes']:
        raise LzfseError("LZFSE block at %d: produced %d bytes, header says n_raw_bytes=%d" % (off, produced, hdr['n_raw_bytes']))
    return lmd_end


def decode_lzvn(buf, off, n_payload, n_raw, out):
    """LZVN block decoder (opcode table of lzvn_decode_base.c).

    Opcode classes by first byte:
      sml_d  2 bytes  L=op>>6  M=((op>>3)&7)+3  D=((op&7)<<8)|b1
      pre_d  1 byte   L, M as sml_d, D = previous D
      lrg_d  3 bytes  L, M as sml_d, D = u16le(b1,b2)
      med_d  3 bytes  (0xA0-0xBF) L=(op>>3)&3  M=(((op&7)<<2)|(b1&3))+3  D=(b1>>2)|(b2<<6)
      lrg_l  0xE0 b1  L=b1+16 literals        sml_l 0xE1-0xEF  L=op&15 literals
      lrg_m  0xF0 b1  M=b1+16 at previous D   sml_m 0xF1-0xFF  M=op&15 at previous D
      nop 0x0E,0x16   eos 0x06   udef 0x1E, 0x70-0x7F, 0xD0-0xDF
    L literal bytes follow the opcode bytes; the match is emitted after them."""
    src = off
    end = off + n_payload
    D = 0
    base = len(out)
    while src < end:
        op = buf[src]
        hi = op & 0xF0
        if op == 0x06:
            src += 1
            break
        if op in (0x0E, 0x16):
            src += 1
            continue
        if op == 0x1E or 0x70 <= op <= 0x7F or 0xD0 <= op <= 0xDF:
            raise LzfseError("LZVN block at %d: undefined opcode 0x%02X at %d" % (off, op, src))
        if op == 0xE0:
            L = buf[src + 1] + 16
            src += 2
            out += buf[src:src + L]
            src += L
            continue
        if 0xE1 <= op <= 0xEF:
            L = op & 15
            src += 1
            out += buf[src:src + L]
            src += L
            continue
        if op == 0xF0:
            Mv = buf[src + 1] + 16
            src += 2
            L = 0
        elif op >= 0xF1:
            Mv = op & 15
            src += 1
            L = 0
        elif 0xA0 <= op <= 0xBF:
            b1 = buf[src + 1]
            b2 = buf[src + 2]
            L = (op >> 3) & 3
            Mv = (((op & 7) << 2) | (b1 & 3)) + 3
            D = (b1 >> 2) | (b2 << 6)
            src += 3
        else:
            L = op >> 6
            Mv = ((op >> 3) & 7) + 3
            low = op & 7
            if low == 7:      # lrg_d
                D = buf[src + 1] | (buf[src + 2] << 8)
                src += 3
            elif low == 6:    # pre_d
                src += 1
            else:             # sml_d
                D = (low << 8) | buf[src + 1]
                src += 2
        if L:
            if src + L > end:
                raise LzfseError("LZVN block at %d: literal run overruns the payload at %d" % (off, src))
            out += buf[src:src + L]
            src += L
        if Mv:
            if D <= 0 or D > len(out) - base + 0 and D > len(out):
                raise LzfseError("LZVN block at %d: match distance %d exceeds output (%d)" % (off, D, len(out)))
            if D > len(out):
                raise LzfseError("LZVN block at %d: match distance %d exceeds output (%d)" % (off, D, len(out)))
            if Mv <= D:
                st = len(out) - D
                out += out[st:st + Mv]
            else:
                st = len(out) - D
                pat = out[st:]
                reps, rem = divmod(Mv, D)
                out += pat * reps + pat[:rem]
    produced = len(out) - base
    if produced != n_raw:
        raise LzfseError("LZVN block at %d: produced %d bytes, header says n_raw_bytes=%d" % (off, produced, n_raw))
    return off + n_payload


def decode_stream(buf, off=0, expected_raw=None, trace=None):
    """Decode a complete LZFSE stream at buf[off:]. Returns (bytes, end_offset, blocks)
    where blocks is a list of (magic, offset, n_raw, n_compressed)."""
    out = bytearray()
    blocks = []
    while True:
        if off + 4 > len(buf):
            raise LzfseError("LZFSE stream truncated at %d: no block magic" % off)
        magic = bytes(buf[off:off + 4])
        if magic == MAGIC_ENDOFSTREAM:
            blocks.append(('bvx$', off, 0, 4))
            off += 4
            break
        if magic == MAGIC_UNCOMPRESSED:
            n_raw = struct.unpack_from('<I', buf, off + 4)[0]
            out += buf[off + 8:off + 8 + n_raw]
            blocks.append(('bvx-', off, n_raw, 8 + n_raw))
            off += 8 + n_raw
        elif magic == MAGIC_COMPRESSED_LZVN:
            n_raw, n_payload = struct.unpack_from('<II', buf, off + 4)
            decode_lzvn(buf, off + 12, n_payload, n_raw, out)
            blocks.append(('bvxn', off, n_raw, 12 + n_payload))
            off += 12 + n_payload
        elif magic in (MAGIC_COMPRESSED_V1, MAGIC_COMPRESSED_V2):
            if magic == MAGIC_COMPRESSED_V2:
                hdr, hsz = decode_v2_header(buf, off)
            else:
                hdr, hsz = decode_v1_header(buf, off)
            end = decode_lzfse_block(buf, off, hdr, out)
            blocks.append((magic.decode(), off, hdr['n_raw_bytes'], end - off))
            off = end
        else:
            raise LzfseError("bad LZFSE block magic %r at offset %d" % (magic, off))
    if expected_raw is not None and len(out) != expected_raw:
        raise LzfseError("LZFSE stream decoded to %d bytes, expected %d" % (len(out), expected_raw))
    return bytes(out), off, blocks



# ============================================================ end LZFSE decoder


class CarError(Exception):
    """A structural problem; `at` is the absolute file offset it concerns."""

    def __init__(self, at, msg):
        self.at = at
        super().__init__("at 0x%06x: %s" % (at, msg))


# ---------------------------------------------------------------- helpers
def be32(d, off, what):
    if off + 4 > len(d):
        raise CarError(off, "%s: need 4 bytes, file ends at 0x%x" % (what, len(d)))
    return struct.unpack_from('>I', d, off)[0]


def be16(d, off, what):
    if off + 2 > len(d):
        raise CarError(off, "%s: need 2 bytes, file ends at 0x%x" % (what, len(d)))
    return struct.unpack_from('>H', d, off)[0]


def le32(d, off, what):
    if off + 4 > len(d):
        raise CarError(off, "%s: need 4 bytes, buffer ends at 0x%x" % (what, len(d)))
    return struct.unpack_from('<I', d, off)[0]


def le16(d, off, what):
    if off + 2 > len(d):
        raise CarError(off, "%s: need 2 bytes, buffer ends at 0x%x" % (what, len(d)))
    return struct.unpack_from('<H', d, off)[0]


def cstr(b):
    i = b.find(b'\0')
    return (b if i < 0 else b[:i]).decode('utf-8', 'replace')


def tag4(b):
    """A 4-byte tag stored as a little-endian u32: bytes 'ISTC' mean 'CTSI'."""
    return bytes(b[:4])[::-1].decode('latin1')


# ---------------------------------------------------------------- name tables
KEY_TOKEN_NAMES = {
    0: 'ThemeLook', 1: 'Element', 2: 'Part', 3: 'Size', 4: 'Direction', 5: 'placeholder',
    6: 'Value', 7: 'Appearance', 8: 'Dimension1', 9: 'Dimension2', 10: 'State', 11: 'Layer',
    12: 'Scale', 13: 'Localization', 14: 'PresentationState', 15: 'Idiom', 16: 'Subtype',
    17: 'Identifier', 18: 'PreviousValue', 19: 'PreviousState', 20: 'HorizontalSizeClass',
    21: 'VerticalSizeClass', 22: 'MemoryLevelClass', 23: 'GraphicsFeatureSetClass',
    24: 'DisplayGamut', 25: 'DeploymentTarget', 26: 'GlyphWeight', 27: 'GlyphSize',
}
# names assetutil prints in "Key Format" (kCRTheme<...>Name); observed for the 9 tokens of the goldens
ASSETUTIL_TOKEN_NAMES = {
    7: 'kCRThemeAppearanceName', 13: 'kCRThemeLocalizationName', 12: 'kCRThemeScaleName',
    15: 'kCRThemeIdiomName', 16: 'kCRThemeSubtypeName', 9: 'kCRThemeDimension2Name',
    17: 'kCRThemeIdentifierName', 1: 'kCRThemeElementName', 2: 'kCRThemePartName',
    # the rest follow the same naming pattern; UNVERIFIED against assetutil output
    0: 'kCRThemeLookName', 3: 'kCRThemeSizeName', 4: 'kCRThemeDirectionName', 6: 'kCRThemeValueName',
    8: 'kCRThemeDimension1Name', 10: 'kCRThemeStateName', 11: 'kCRThemeLayerName',
    14: 'kCRThemePresentationStateName', 18: 'kCRThemePreviousValueName', 19: 'kCRThemePreviousStateName',
    20: 'kCRThemeSizeClassHorizontalName', 21: 'kCRThemeSizeClassVerticalName', 22: 'kCRThemeMemoryClassName',
    23: 'kCRThemeGraphicsClassName', 24: 'kCRThemeDisplayGamutName', 25: 'kCRThemeDeploymentTargetName',
}
IDIOM_NAMES = {0: 'universal', 1: 'phone', 2: 'pad', 3: 'tv', 4: 'car', 5: 'watch', 6: 'marketing'}
LAYOUT_NAMES = {
    0x0A: 'OnePartFixedSize', 0x0B: 'OnePartTile', 0x0C: 'OnePartScale',
    0x14: 'ThreePartHTile', 0x15: 'ThreePartHScale', 0x16: 'ThreePartHUniform',
    0x17: 'ThreePartVTile', 0x18: 'ThreePartVScale', 0x19: 'ThreePartVUniform',
    0x1E: 'NinePartTile', 0x1F: 'NinePartScale', 0x20: 'NinePartHorizontalUniformVerticalScale',
    0x21: 'NinePartHorizontalScaleVerticalUniform', 0x22: 'NinePartEdgesOnly',
    0x28: 'ManyPartLayoutUnknown', 0x32: 'AnimationFilmstrip',
    0x3E8: 'Data', 0x3E9: 'ExternalLink', 0x3EA: 'LayerStack', 0x3EB: 'InternalReference',
    0x3EC: 'PackedImage', 0x3ED: 'NameList', 0x3EE: 'UnknownAddObject', 0x3EF: 'Texture',
    0x3F0: 'TextureImage', 0x3F1: 'Color', 0x3F2: 'MultisizeImage', 0x3F4: 'LayerReference',
    0x3F5: 'ContentRendition', 0x3F6: 'RecognitionObject',
}
COMPRESSION_NAMES = {0: 'uncompressed', 1: 'rle', 2: 'zip', 3: 'lzvn', 4: 'lzfse', 5: 'jpeg-lzfse',
                     6: 'blurred', 7: 'astc', 8: 'palette-img', 9: 'deepmap-lzfse', 10: 'unknown-10', 11: 'deepmap-2'}
TLV_NAMES = {0x3E9: 'Slices', 0x3EA: 'Unknown-0x3EA', 0x3EB: 'Metrics', 0x3EC: 'BlendModeAndOpacity', 0x3ED: 'UTI',
             0x3EE: 'EXIFOrientation', 0x3EF: 'RowBytes(UNVERIFIED name; value == width*4 in goldens)',
             0x3F0: 'ExternalTags', 0x3F1: 'Frame'}
COLORSPACE_NAMES = {0: 'unspecified', 1: 'srgb', 2: 'monochrome', 3: 'displayP3', 4: 'extendedRangeSRGB',
                    5: 'extendedLinearSRGB', 6: 'extendedGray'}  # only 1 verified against assetutil


# ---------------------------------------------------------------- BOM container
class Bom:
    HEADER_PAD = 0x200

    def __init__(self, data):
        self.d = data
        d = data
        if len(d) < 32:
            raise CarError(0, "file is %d bytes, shorter than the 32-byte BOM header" % len(d))
        if d[:8] != b'BOMStore':
            raise CarError(0, "bad magic %r, expected b'BOMStore'" % bytes(d[:8]))
        self.version = be32(d, 8, 'BOM version')
        self.n_blocks = be32(d, 12, 'BOM numberOfBlocks')
        self.index_off = be32(d, 16, 'BOM indexOffset')
        self.index_len = be32(d, 20, 'BOM indexLength')
        self.vars_off = be32(d, 24, 'BOM varsOffset')
        self.vars_len = be32(d, 28, 'BOM varsLength')
        if self.version != 1:
            raise CarError(8, "BOM version %d, expected 1" % self.version)
        if self.index_off + self.index_len > len(d):
            raise CarError(16, "index 0x%x+%d runs past end of file (0x%x)" % (self.index_off, self.index_len, len(d)))
        if self.vars_off + self.vars_len > len(d):
            raise CarError(24, "vars 0x%x+%d runs past end of file (0x%x)" % (self.vars_off, self.vars_len, len(d)))
        # block table
        io = self.index_off
        self.n_ptrs = be32(d, io, 'block table count')
        need = io + 4 + 8 * self.n_ptrs
        if need > len(d):
            raise CarError(io, "block table claims %d pointers (needs up to 0x%x) but file ends at 0x%x" % (self.n_ptrs, need, len(d)))
        self.blocks = []
        for i in range(self.n_ptrs):
            a = be32(d, io + 4 + 8 * i, 'block %d address' % i)
            l = be32(d, io + 8 + 8 * i, 'block %d length' % i)
            if l and a + l > len(d):
                raise CarError(io + 4 + 8 * i, "block %d = 0x%x+%d runs past end of file" % (i, a, l))
            self.blocks.append((a, l))
        if self.blocks[0] != (0, 0):
            raise CarError(io + 4, "block table entry 0 must be null, is 0x%x+%d" % self.blocks[0])
        nonnull = sum(1 for a, l in self.blocks if l)
        if nonnull != self.n_blocks:
            raise CarError(12, "header numberOfBlocks=%d but the block table has %d non-null entries" % (self.n_blocks, nonnull))
        # free list
        fo = io + 4 + 8 * self.n_ptrs
        self.free_off = fo
        self.n_free = be32(d, fo, 'free list count')
        self.free = []
        for i in range(self.n_free):
            self.free.append((be32(d, fo + 4 + 8 * i, 'free %d addr' % i), be32(d, fo + 8 + 8 * i, 'free %d len' % i)))
        self.free_end = fo + 4 + 8 * self.n_free
        self.index_tail = bytes(d[self.free_end:io + self.index_len])
        # vars
        vo = self.vars_off
        n = be32(d, vo, 'vars count')
        p = vo + 4
        self.vars = []
        for i in range(n):
            idx = be32(d, p, 'var %d index' % i)
            ln = d[p + 4]
            name = bytes(d[p + 5:p + 5 + ln]).decode('ascii', 'replace')
            self.vars.append((name, idx, p))
            p += 5 + ln
        self.vars_end = p
        if p != vo + self.vars_len:
            raise CarError(vo, "vars block parsed to 0x%x but varsLength says end is 0x%x" % (p, vo + self.vars_len))
        self.var_index = {name: idx for name, idx, _ in self.vars}

    def block(self, i, what='block'):
        if i >= len(self.blocks):
            raise CarError(self.index_off, "%s: block index %d beyond the table (%d entries)" % (what, i, len(self.blocks)))
        a, l = self.blocks[i]
        if l == 0 and i != 0:
            raise CarError(self.index_off + 4 + 8 * i, "%s: block %d is null" % (what, i))
        return a, l, self.d[a:a + l]

    def named(self, name):
        if name not in self.var_index:
            raise CarError(self.vars_off, "no var named %r (have %s)" % (name, [v[0] for v in self.vars]))
        return self.block(self.var_index[name], name)


class Tree:
    """A BOM 'tree' var: header block + leaf paths block(s)."""

    def __init__(self, bom, name):
        self.bom = bom
        self.name = name
        a, l, b = bom.named(name)
        self.hdr_off, self.hdr_len = a, l
        if b[:4] != b'tree':
            raise CarError(a, "%s: tree header tag %r, expected b'tree'" % (name, bytes(b[:4])))
        if l < 21:
            raise CarError(a, "%s: tree header is %d bytes, need >= 21" % (name, l))
        self.version = be32(b, 4, 'tree version')
        self.child = be32(b, 8, 'tree child')
        self.block_size = be32(b, 12, 'tree blockSize')
        self.path_count = be32(b, 16, 'tree pathCount')
        self.flag = b[20]
        self.key_size = be32(b, 21, 'tree keySize') if l >= 25 else None
        self.tail = be32(b, 25, 'tree tail') if l >= 29 else None
        if self.version != 1:
            raise CarError(a + 4, "%s: tree version %d, expected 1" % (name, self.version))
        self.inline_keys = (self.flag == 1)
        # walk to the first leaf
        seen = set()
        pa, pl, pb = bom.block(self.child, name + ' paths')
        depth = 0
        while True:
            is_leaf = be16(pb, 0, 'paths isLeaf')
            if is_leaf:
                break
            cnt = be16(pb, 2, 'paths count')
            if cnt == 0:
                raise CarError(pa, "%s: branch node with zero children" % name)
            nxt = be32(pb, 12, 'paths first child')
            if nxt in seen or depth > 64:
                raise CarError(pa, "%s: cycle in tree branches" % name)
            seen.add(nxt)
            pa, pl, pb = bom.block(nxt, name + ' paths')
            depth += 1
        self.leaves = []       # (addr, len, count, forward, backward, entries[(value_idx, key_idx)])
        self.entries = []      # dicts
        while True:
            cnt = be16(pb, 2, 'paths count')
            fwd = be32(pb, 4, 'paths forward')
            bwd = be32(pb, 8, 'paths backward')
            ents = []
            for k in range(cnt):
                eo = 12 + 8 * k
                if eo + 8 > pl:
                    raise CarError(pa + eo, "%s: leaf claims %d entries but block is only %d bytes" % (name, cnt, pl))
                vi = be32(pb, eo, 'entry value index')
                ki = be32(pb, eo + 4, 'entry key index')
                ents.append((vi, ki, pa + eo))
            self.leaves.append((pa, pl, cnt, fwd, bwd, ents))
            for vi, ki, eo in ents:
                e = {'entry_off': eo, 'value_index': vi, 'key_index': ki}
                va, vl, vb = bom.block(vi, name + ' value')
                e['value_off'], e['value_len'], e['value'] = va, vl, bytes(vb)
                if self.inline_keys:
                    e['key_off'], e['key_len'], e['key'] = eo + 4, 4, None
                    e['key_inline'] = ki
                else:
                    ka, kl, kb = bom.block(ki, name + ' key')
                    e['key_off'], e['key_len'], e['key'] = ka, kl, bytes(kb)
                self.entries.append(e)
            if fwd == 0:
                break
            if fwd in seen:
                raise CarError(pa + 4, "%s: cycle in leaf chain" % name)
            seen.add(fwd)
            pa, pl, pb = bom.block(fwd, name + ' paths')
        if self.path_count != len(self.entries):
            raise CarError(a + 16, "%s: tree pathCount=%d but %d entries found" % (name, self.path_count, len(self.entries)))

    def leaf_inline_key_copy(self, leaf):
        """The bytes following the entry table inside a leaf block (actool writes a copy of the keys there)."""
        pa, pl, cnt, fwd, bwd, ents = leaf
        start = 12 + 8 * cnt
        return pa + start, self.bom.d[pa + start:pa + pl]


# ---------------------------------------------------------------- CAR blocks
def parse_carheader(bom):
    a, l, b = bom.named('CARHEADER')
    if l < 436:
        raise CarError(a, "CARHEADER is %d bytes, expected 436" % l)
    if tag4(b) != 'CTAR':
        raise CarError(a, "CARHEADER tag %r (LE 'CTAR' expected: bytes 'RATC')" % bytes(b[:4]))
    h = {'off': a, 'len': l}
    h['coreui_version'] = le32(b, 4, 'coreuiVersion')
    h['storage_version'] = le32(b, 8, 'storageVersion')
    h['storage_timestamp'] = le32(b, 12, 'storageTimestamp')
    h['rendition_count'] = le32(b, 16, 'renditionCount')
    h['main_version'] = cstr(b[20:148])
    h['version_string'] = cstr(b[148:404])
    h['uuid'] = bytes(b[404:420])
    h['associated_checksum'] = le32(b, 420, 'associatedChecksum')
    h['schema_version'] = le32(b, 424, 'schemaVersion')
    h['colorspace_id'] = le32(b, 428, 'colorSpaceID')
    h['key_semantics'] = le32(b, 432, 'keySemantics')
    h['raw'] = bytes(b)
    return h


def parse_metadata(bom):
    a, l, b = bom.named('EXTENDED_METADATA')
    if l < 1028:
        raise CarError(a, "EXTENDED_METADATA is %d bytes, expected 1028" % l)
    # NB: unlike every other tag in the file, this one is stored as the literal
    # bytes "META" (not byte-swapped): goldens have 4d 45 54 41 at the block start.
    if bytes(b[:4]) != b'META':
        raise CarError(a, "EXTENDED_METADATA tag %r, expected the literal bytes b'META'" % bytes(b[:4]))
    return {'off': a, 'len': l,
            'thinning_arguments': cstr(b[4:260]),
            'deployment_platform_version': cstr(b[260:516]),
            'deployment_platform': cstr(b[516:772]),
            'authoring_tool': cstr(b[772:1028])}


def parse_keyformat(bom):
    a, l, b = bom.named('KEYFORMAT')
    if tag4(b) != 'kfmt':
        raise CarError(a, "KEYFORMAT tag %r (LE 'kfmt' expected: bytes 'tmfk')" % bytes(b[:4]))
    version = le32(b, 4, 'kfmt version')
    n = le32(b, 8, 'kfmt token count')
    if 12 + 4 * n != l:
        raise CarError(a + 8, "KEYFORMAT has %d tokens but block is %d bytes (expected %d)" % (n, l, 12 + 4 * n))
    tokens = [le32(b, 12 + 4 * i, 'kfmt token %d' % i) for i in range(n)]
    return {'off': a, 'len': l, 'version': version, 'tokens': tokens}


def parse_rendition_key(kb, off, tokens):
    if len(kb) != 2 * len(tokens):
        raise CarError(off, "rendition key is %d bytes, KEYFORMAT has %d tokens (expected %d bytes)" % (len(kb), len(tokens), 2 * len(tokens)))
    vals = struct.unpack('<%dH' % len(tokens), kb)
    return dict(zip(tokens, vals))


def parse_facet_value(vb, off):
    if len(vb) < 6:
        raise CarError(off, "facet value is %d bytes, need >= 6" % len(vb))
    hx, hy, n = struct.unpack_from('<HHH', vb, 0)
    if 6 + 4 * n != len(vb):
        raise CarError(off + 4, "facet value has %d attributes but block is %d bytes (expected %d)" % (n, len(vb), 6 + 4 * n))
    attrs = [struct.unpack_from('<HH', vb, 6 + 4 * i) for i in range(n)]
    return {'hotspot': (hx, hy), 'attrs': attrs, 'attr_map': dict(attrs)}


def parse_bitmap_value(vb, off):
    if len(vb) < 16:
        raise CarError(off, "BITMAPKEYS value is %d bytes, need >= 16" % len(vb))
    f0, f1, blen, n = struct.unpack_from('<IIII', vb, 0)
    if 12 + blen != len(vb) or blen != 4 + 4 * n:
        raise CarError(off + 8, "BITMAPKEYS value: fields (%d,%d,len=%d,count=%d) inconsistent with block length %d" % (f0, f1, blen, n, len(vb)))
    masks = [struct.unpack_from('<i', vb, 16 + 4 * i)[0] for i in range(n)]
    return {'f0': f0, 'f1': f1, 'byte_len': blen, 'count': n, 'masks': masks}


# ---------------------------------------------------------------- renditions
def parse_tlvs(vb, base, start, length):
    tlvs = []
    p = start
    end = start + length
    while p < end:
        if p + 8 > end:
            raise CarError(base + p, "TLV header overruns the TLV area (ends at +%d)" % end)
        t = le32(vb, p, 'tlv type')
        ln = le32(vb, p + 4, 'tlv length')
        if p + 8 + ln > end:
            raise CarError(base + p, "TLV 0x%x length %d overruns the TLV area (ends at +%d)" % (t, ln, end))
        raw = bytes(vb[p + 8:p + 8 + ln])
        dec = None
        if t == 0x3E9 and ln >= 4:
            n = le32(raw, 0, 'slices')
            dec = {'count': n, 'slices': [struct.unpack_from('<IIII', raw, 4 + 16 * i) for i in range(n)] if ln == 4 + 16 * n else raw.hex()}
        elif t == 0x3EB and ln >= 4:
            n = le32(raw, 0, 'metrics')
            dec = {'count': n, 'metrics': [dict(zip(('top', 'left', 'bottom', 'right', 'width', 'height'), struct.unpack_from('<IIIIII', raw, 4 + 24 * i))) for i in range(n)] if ln == 4 + 24 * n else raw.hex()}
        elif t == 0x3EC and ln == 8:
            dec = {'blend_mode': le32(raw, 0, 'blend'), 'opacity': struct.unpack_from('<f', raw, 4)[0]}
        elif t == 0x3EE and ln == 4:
            dec = {'orientation': le32(raw, 0, 'exif')}
        elif t == 0x3EF and ln == 4:
            dec = {'value': le32(raw, 0, 'rowbytes')}
        elif t == 0x3ED:
            dec = {'uti': cstr(raw)}
        tlvs.append({'off': base + p, 'type': t, 'len': ln, 'raw': raw, 'decoded': dec})
        p += 8 + ln
    if p != end:
        raise CarError(base + p, "TLV area ends at +%d, expected +%d" % (p, end))
    return tlvs


def parse_csi(vb, base):
    """Parse a rendition value block: CSI header (184 bytes) + TLVs + payload."""
    if len(vb) < 184:
        raise CarError(base, "rendition value is %d bytes, CSI header needs 184" % len(vb))
    if tag4(vb) != 'CTSI':
        raise CarError(base, "rendition tag %r (LE 'CTSI' expected: bytes 'ISTC')" % bytes(vb[:4]))
    c = {'off': base, 'len': len(vb)}
    c['version'] = le32(vb, 4, 'csi version')
    c['flags'] = le32(vb, 8, 'csi flags')
    c['width'] = le32(vb, 12, 'csi width')
    c['height'] = le32(vb, 16, 'csi height')
    c['scale_factor'] = le32(vb, 20, 'csi scaleFactor')
    c['pixel_format'] = tag4(vb[24:28]) if any(vb[24:28]) else ''
    c['pixel_format_raw'] = le32(vb, 24, 'csi pixelFormat')
    cs = le32(vb, 28, 'csi colorSpace')
    c['colorspace_id'] = cs & 0xF
    c['colorspace_reserved'] = cs >> 4
    c['modtime'] = le32(vb, 32, 'csi modtime')
    c['layout'] = le16(vb, 36, 'csi layout')
    c['layout_zero'] = le16(vb, 38, 'csi layout pad')
    c['name'] = cstr(vb[40:168])
    c['name_raw'] = bytes(vb[40:168])
    c['tlv_len'] = le32(vb, 168, 'csi tlvLength')
    c['bitmaplist_unknown'] = le32(vb, 172, 'csi unknown')
    c['bitmaplist_zero'] = le32(vb, 176, 'csi zero')
    c['rendition_len'] = le32(vb, 180, 'csi renditionLength')
    if 184 + c['tlv_len'] + c['rendition_len'] != len(vb):
        raise CarError(base + 168, "184 + tlvLength(%d) + renditionLength(%d) = %d but the value block is %d bytes"
                       % (c['tlv_len'], c['rendition_len'], 184 + c['tlv_len'] + c['rendition_len'], len(vb)))
    c['tlvs'] = parse_tlvs(vb, base, 184, c['tlv_len'])
    c['tlv_map'] = {t['type']: t for t in c['tlvs']}
    p = 184 + c['tlv_len']
    c['payload_off'] = base + p
    c['payload'] = bytes(vb[p:])
    c['payload_tag'] = tag4(vb[p:p + 4]) if len(vb) >= p + 4 else ''
    pl = c['payload']
    pb = c['payload_off']
    if c['payload_tag'] == 'CELM':
        celm = {'version': le32(pl, 4, 'CELM version'), 'compression': le32(pl, 8, 'CELM compression')}
        v = celm['version']
        if v == 3:
            n = le32(pl, 12, 'CELM chunk count')
            celm['chunk_count'] = n
            chunks = []
            q = 16
            for k in range(n):
                if q + 20 > len(pl):
                    raise CarError(pb + q, "CELM v3 chunk %d header overruns the payload" % k)
                if tag4(pl[q:q + 4]) != 'CBCK':
                    raise CarError(pb + q, "CELM v3 chunk %d tag %r (LE 'CBCK' expected: bytes 'KCBC')" % (k, bytes(pl[q:q + 4])))
                f1 = le32(pl, q + 4, 'chunk f1')
                f2 = le32(pl, q + 8, 'chunk f2')
                rows = le32(pl, q + 12, 'chunk rows')
                clen = le32(pl, q + 16, 'chunk compressed length')
                if q + 20 + clen > len(pl):
                    raise CarError(pb + q + 16, "CELM v3 chunk %d claims %d compressed bytes, payload has %d left" % (k, clen, len(pl) - q - 20))
                chunks.append({'off': pb + q, 'f1': f1, 'f2': f2, 'rows': rows, 'comp_len': clen,
                               'data_off': pb + q + 20, 'data': pl[q + 20:q + 20 + clen]})
                q += 20 + clen
            if q != len(pl):
                raise CarError(pb + q, "CELM v3 chunks end at payload+%d but the payload is %d bytes" % (q, len(pl)))
            celm['chunks'] = chunks
        elif v in (0, 2):
            celm['raw_len'] = le32(pl, 12, 'CELM rawDataLength')
            celm['data_off'] = pb + 16
            celm['data'] = pl[16:16 + celm['raw_len']]
            if 16 + celm['raw_len'] != len(pl):
                raise CarError(pb + 12, "CELM v%d rawDataLength %d but payload has %d bytes after the header" % (v, celm['raw_len'], len(pl) - 16))
        else:
            celm['unparsed'] = True
        c['celm'] = celm
    elif c['payload_tag'] == 'MSIS':
        ms = {'version': le32(pl, 4, 'MSIS version'), 'count': le32(pl, 8, 'MSIS count')}
        n = ms['count']
        if 12 + 12 * n != len(pl):
            raise CarError(pb + 8, "MSIS has %d entries but payload is %d bytes (expected %d)" % (n, len(pl), 12 + 12 * n))
        ms['entries'] = [dict(zip(('width', 'height', 'index'), struct.unpack_from('<III', pl, 12 + 12 * i))) for i in range(n)]
        c['msis'] = ms
    elif c['payload_tag'] == 'RAWD':
        c['rawd'] = {'version': le32(pl, 4, 'RAWD version'), 'len': le32(pl, 8, 'RAWD length'), 'data_off': pb + 12}
    return c


def decode_pixels(csi):
    """Decode the CELM payload of an image rendition to raw bytes (BGRA rows, top-down).
    Returns (raw_bytes, notes)."""
    celm = csi.get('celm')
    if not celm:
        raise CarError(csi['payload_off'], "no CELM payload to decode (tag %r)" % csi['payload_tag'])
    comp = celm['compression']
    row_bytes = csi['tlv_map'][0x3EF]['decoded']['value'] if 0x3EF in csi['tlv_map'] else csi['width'] * 4
    out = bytearray()
    notes = []
    if celm['version'] == 3:
        total_rows = 0
        for k, ch in enumerate(celm['chunks']):
            if comp == 4:
                try:
                    raw, end, blocks = decode_stream(ch['data'])
                except LzfseError as e:
                    raise CarError(ch['data_off'], "chunk %d LZFSE decode failed: %s" % (k, e))
                if end != len(ch['data']):
                    raise CarError(ch['data_off'] + end, "chunk %d: LZFSE stream ended after %d of %d bytes" % (k, end, len(ch['data'])))
                notes.append("chunk %d @0x%06x: rows=%d lzfse blocks=%s -> %d bytes" % (k, ch['off'], ch['rows'], [(b[0], b[2]) for b in blocks], len(raw)))
            elif comp == 0:
                raw = bytes(ch['data'])
            else:
                raise CarError(ch['data_off'], "chunk %d: compression %d (%s) not supported by this tool" % (k, comp, COMPRESSION_NAMES.get(comp, '?')))
            if len(raw) != ch['rows'] * row_bytes:
                raise CarError(ch['data_off'], "chunk %d decoded to %d bytes, but rows(%d) * rowBytes(%d) = %d" % (k, len(raw), ch['rows'], row_bytes, ch['rows'] * row_bytes))
            out += raw
            total_rows += ch['rows']
        if total_rows != csi['height']:
            raise CarError(csi['payload_off'], "chunks cover %d rows, CSI height is %d" % (total_rows, csi['height']))
    elif celm['version'] in (0, 2):
        if comp == 4:
            raw, end, blocks = decode_stream(celm['data'])
        elif comp == 0:
            raw = bytes(celm['data'])
        else:
            raise CarError(celm['data_off'], "compression %d not supported by this tool" % comp)
        out += raw
    else:
        raise CarError(csi['payload_off'], "CELM version %d not supported by this tool" % celm['version'])
    if len(out) != csi['height'] * row_bytes:
        raise CarError(csi['payload_off'], "decoded %d bytes, expected height(%d) * rowBytes(%d) = %d" % (len(out), csi['height'], row_bytes, csi['height'] * row_bytes))
    return bytes(out), notes


# ---------------------------------------------------------------- whole-catalog model
class Car:
    def __init__(self, data, decode=True):
        self.d = data
        self.bom = Bom(data)
        self.header = parse_carheader(self.bom)
        self.meta = parse_metadata(self.bom) if 'EXTENDED_METADATA' in self.bom.var_index else None
        self.keyformat = parse_keyformat(self.bom)
        self.tokens = self.keyformat['tokens']
        self.trees = {}
        for name in ('APPEARANCEKEYS', 'FACETKEYS', 'BITMAPKEYS', 'RENDITIONS'):
            if name in self.bom.var_index:
                self.trees[name] = Tree(self.bom, name)
        self.appearances = []
        if 'APPEARANCEKEYS' in self.trees:
            for e in self.trees['APPEARANCEKEYS'].entries:
                if e['value_len'] != 2:
                    raise CarError(e['value_off'], "APPEARANCEKEYS value is %d bytes, expected 2 (u16)" % e['value_len'])
                a = dict(e)
                a['name'] = e['key'].decode('utf-8', 'replace')
                a['appearance_value'] = le16(e['value'], 0, 'appearance')
                self.appearances.append(a)
        self.facets = []
        if 'FACETKEYS' in self.trees:
            for e in self.trees['FACETKEYS'].entries:
                fv = parse_facet_value(e['value'], e['value_off'])
                self.facets.append({'name': e['key'].decode('utf-8', 'replace'), **fv, **e})
        self.facet_by_identifier = {}
        for f in self.facets:
            ident = f['attr_map'].get(17)
            if ident is not None:
                self.facet_by_identifier[ident] = f
        self.bitmapkeys = []
        if 'BITMAPKEYS' in self.trees:
            for e in self.trees['BITMAPKEYS'].entries:
                bv = parse_bitmap_value(e['value'], e['value_off'])
                self.bitmapkeys.append({'identifier': e.get('key_inline'), **bv, **e})
        self.renditions = []
        if 'RENDITIONS' in self.trees:
            for e in self.trees['RENDITIONS'].entries:
                key = parse_rendition_key(e['key'], e['key_off'], self.tokens)
                csi = parse_csi(e['value'], e['value_off'])
                r = dict(e)
                r['key_bytes'] = e['key']
                r['key'] = key
                r['csi'] = csi
                r['sha256'] = hashlib.sha256(e['value']).hexdigest().upper()
                r['pixels'] = None
                r['pixel_notes'] = []
                r['decode_error'] = None
                if decode and 'celm' in csi:
                    try:
                        r['pixels'], r['pixel_notes'] = decode_pixels(csi)
                    except (CarError, LzfseError) as ex:
                        # structural decode of the file is complete at this point; a payload
                        # this tool cannot decompress is reported, not fatal (see --require-decode)
                        r['decode_error'] = str(ex)
                self.renditions.append(r)
        if self.header['rendition_count'] != len(self.renditions):
            raise CarError(self.header['off'] + 16, "CARHEADER renditionCount=%d but RENDITIONS has %d entries" % (self.header['rendition_count'], len(self.renditions)))

    # ---- assetutil view
    def rendition_asset_view(self, r, with_pixels=True):
        """The fields `xcrun assetutil --info` prints for this rendition, derived from the bytes."""
        key, csi = r['key'], r['csi']
        idiom = key.get(15, 0)
        ident = key.get(17)
        facet = self.facet_by_identifier.get(ident)
        v = {'Idiom': IDIOM_NAMES.get(idiom, 'idiom-%d' % idiom),
             'Name': facet['name'] if facet else None,
             'NameIdentifier': ident,
             'SizeOnDisk': r['value_len'],
             'SHA1Digest': r['sha256']}
        if csi['layout'] == 0x3F2:
            v['AssetType'] = 'MultiSized Image'
            v['Scale'] = key.get(12, 1)
            v['Sizes'] = ['%dx%d index:%d idiom:%s' % (e['width'], e['height'], e['index'], v['Idiom']) for e in csi['msis']['entries']]
        else:
            # element 85 renditions print as "Icon Image" in every golden; other elements UNVERIFIED
            v['AssetType'] = 'Icon Image' if key.get(1) == 85 else 'Image'
            v['Scale'] = csi['scale_factor'] // 100 if csi['scale_factor'] % 100 == 0 else csi['scale_factor'] / 100
            v['PixelWidth'] = csi['width']
            v['PixelHeight'] = csi['height']
            v['RenditionName'] = csi['name']
            v['Encoding'] = csi['pixel_format']
            v['Colorspace'] = COLORSPACE_NAMES.get(csi['colorspace_id'], 'colorspace-%d' % csi['colorspace_id'])
            if 'celm' in csi:
                v['Compression'] = COMPRESSION_NAMES.get(csi['celm']['compression'], 'compression-%d' % csi['celm']['compression'])
            if csi['pixel_format'] == 'ARGB':
                v['BitsPerComponent'] = 8
                v['ColorModel'] = 'RGB'
            if 9 in key:
                v['Icon Index'] = key[9]
            if with_pixels and r['pixels'] is not None and csi['pixel_format'] == 'ARGB':
                px = r['pixels']
                v['Opaque'] = all(px[i] == 255 for i in range(3, len(px), 4))
        return v

    def header_asset_view(self):
        h, m = self.header, self.meta
        v = {'CoreUIVersion': h['coreui_version'], 'StorageVersion': h['storage_version'], 'SchemaVersion': h['schema_version'],
             'MainVersion': h['main_version'], 'AssetStorageVersion': h['version_string'],
             'Key Format': [ASSETUTIL_TOKEN_NAMES.get(t, 'token-%d' % t) for t in self.tokens],
             'Appearances': {a['name']: a['appearance_value'] for a in self.appearances}}
        if m:
            v['Platform'] = m['deployment_platform']
            v['PlatformVersion'] = m['deployment_platform_version']
            v['Authoring Tool'] = m['authoring_tool']
            if m['thinning_arguments']:
                v['ThinningArguments'] = m['thinning_arguments']
        return v


# ---------------------------------------------------------------- dump
def hexline(b):
    return ' '.join('%02x' % x for x in b)


def dump(car, out=sys.stdout, hexdump=False):
    d = car.d
    bom = car.bom
    P = lambda *a: print(*a, file=out)
    P("file: %d bytes (0x%x)" % (len(d), len(d)))
    P("== BOM header @0x000000")
    P("  [0x000000] magic            = %r" % bytes(d[:8]))
    P("  [0x000008] version          = %d" % bom.version)
    P("  [0x00000c] numberOfBlocks   = %d (non-null block table entries)" % bom.n_blocks)
    P("  [0x000010] indexOffset      = 0x%x" % bom.index_off)
    P("  [0x000014] indexLength      = %d  (indexOffset+indexLength = 0x%x, file end 0x%x)" % (bom.index_len, bom.index_off + bom.index_len, len(d)))
    P("  [0x000018] varsOffset       = 0x%x" % bom.vars_off)
    P("  [0x00001c] varsLength       = %d" % bom.vars_len)
    pad = d[32:min(Bom.HEADER_PAD, len(d))]
    P("  [0x000020..0x%x] header padding: %d bytes, all zero: %s" % (min(Bom.HEADER_PAD, len(d)), len(pad), set(pad) <= {0}))
    P("== block table @0x%06x: %d pointers" % (bom.index_off, bom.n_ptrs))
    used = [(a, l, i) for i, (a, l) in enumerate(bom.blocks) if l]
    names = {idx: n for n, idx, _ in bom.vars}
    prev = Bom.HEADER_PAD
    for a, l, i in sorted(used):
        gap = d[prev:a] if a >= prev else b''
        P("  [0x%06x] block %3d @0x%06x len %7d -> 0x%06x  gap-before=%2d zero=%s align16=%s %s"
          % (bom.index_off + 4 + 8 * i, i, a, l, a + l, len(gap), set(gap) <= {0}, a % 16 == 0, names.get(i, '')))
        prev = a + l
    P("  non-null entries: %d; null entries after the last used: %d" % (len(used), bom.n_ptrs - 1 - max(i for _, _, i in used)))
    P("== free list @0x%06x: count=%d %s" % (bom.free_off, bom.n_free, bom.free))
    P("  index tail after free list: %d bytes = %s" % (len(bom.index_tail), bom.index_tail.hex() or '(none)'))
    P("== vars @0x%06x: count=%d, parsed end 0x%06x" % (bom.vars_off, len(bom.vars), bom.vars_end))
    for n, idx, off in bom.vars:
        P("  [0x%06x] index=%2d len=%2d name=%s" % (off, idx, len(n), n))
    h = car.header
    P("== CARHEADER (block %d) @0x%06x len %d" % (bom.var_index['CARHEADER'], h['off'], h['len']))
    o = h['off']
    P("  [0x%06x+000] tag              = 'CTAR' (bytes %r)" % (o, h['raw'][:4]))
    P("  [0x%06x+004] coreuiVersion    = %d" % (o, h['coreui_version']))
    P("  [0x%06x+008] storageVersion   = %d" % (o, h['storage_version']))
    P("  [0x%06x+012] storageTimestamp = %d" % (o, h['storage_timestamp']))
    P("  [0x%06x+016] renditionCount   = %d" % (o, h['rendition_count']))
    P("  [0x%06x+020] mainVersionString[128] = %r" % (o, h['main_version']))
    P("  [0x%06x+148] versionString[256]     = %r" % (o, h['version_string']))
    P("  [0x%06x+404] uuid[16]         = %s" % (o, h['uuid'].hex()))
    P("  [0x%06x+420] associatedChecksum = 0x%x" % (o, h['associated_checksum']))
    P("  [0x%06x+424] schemaVersion    = %d" % (o, h['schema_version']))
    P("  [0x%06x+428] colorSpaceID     = %d" % (o, h['colorspace_id']))
    P("  [0x%06x+432] keySemantics     = %d" % (o, h['key_semantics']))
    if car.meta:
        m = car.meta
        o = m['off']
        P("== EXTENDED_METADATA (block %d) @0x%06x len %d" % (bom.var_index['EXTENDED_METADATA'], o, m['len']))
        P("  [0x%06x+000] tag bytes 'META' (NOT byte-swapped, unlike CTAR/kfmt/CTSI/CELM/CBCK/MSIS); +004 thinningArguments[256] = %r" % (o, m['thinning_arguments']))
        P("  [0x%06x+260] deploymentPlatformVersion[256] = %r" % (o, m['deployment_platform_version']))
        P("  [0x%06x+516] deploymentPlatform[256]        = %r" % (o, m['deployment_platform']))
        P("  [0x%06x+772] authoringTool[256]             = %r" % (o, m['authoring_tool']))
    k = car.keyformat
    P("== KEYFORMAT (block %d) @0x%06x len %d: tag 'kfmt' version=%d tokens=%d" % (bom.var_index['KEYFORMAT'], k['off'], k['len'], k['version'], len(k['tokens'])))
    for i, t in enumerate(k['tokens']):
        P("  [0x%06x] token[%d] = %2d %s" % (k['off'] + 12 + 4 * i, i, t, KEY_TOKEN_NAMES.get(t, '?')))
    for name in ('APPEARANCEKEYS', 'FACETKEYS', 'BITMAPKEYS', 'RENDITIONS'):
        if name not in car.trees:
            P("== %s: absent" % name)
            continue
        t = car.trees[name]
        P("== %s tree header (block %d) @0x%06x len %d: version=%d child=%d blockSize=%d pathCount=%d flag@+20=%d keySize@+21=%s tail@+25=%s"
          % (name, bom.var_index[name], t.hdr_off, t.hdr_len, t.version, t.child, t.block_size, t.path_count, t.flag, t.key_size, t.tail))
        for (pa, pl, cnt, fwd, bwd, ents) in t.leaves:
            P("   leaf paths block @0x%06x len %d: isLeaf=1 count=%d forward=%d backward=%d" % (pa, pl, cnt, fwd, bwd))
            for vi, ki, eo in ents:
                P("     [0x%06x] value_index=%d key_index=%s" % (eo, vi, ('0x%x (inline key)' % ki) if t.inline_keys else str(ki)))
            copy_off, copy = t.leaf_inline_key_copy((pa, pl, cnt, fwd, bwd, ents))
            nz = [i for i, x in enumerate(copy) if x]
            if nz:
                P("     [0x%06x] bytes after the entry table: %d, nonzero span +%d..+%d: %s" % (copy_off, len(copy), nz[0], nz[-1] + 1, copy[nz[0]:nz[-1] + 1].hex()))
            else:
                P("     [0x%06x] bytes after the entry table: %d, all zero" % (copy_off, len(copy)))
        for e in t.entries:
            if name == 'APPEARANCEKEYS':
                P("   key @0x%06x %r -> value @0x%06x u16 %d" % (e['key_off'], e['key'], e['value_off'], le16(e['value'], 0, 'a')))
            elif name == 'FACETKEYS':
                fv = parse_facet_value(e['value'], e['value_off'])
                P("   key @0x%06x %r -> value @0x%06x len %d: hotspot=%s nattrs=%d attrs=%s"
                  % (e['key_off'], e['key'], e['value_off'], e['value_len'], fv['hotspot'], len(fv['attrs']),
                     ', '.join('%s(%d)=%d' % (KEY_TOKEN_NAMES.get(a, '?'), a, v) for a, v in fv['attrs'])))
            elif name == 'BITMAPKEYS':
                bv = parse_bitmap_value(e['value'], e['value_off'])
                P("   inline key (NameIdentifier) %d -> value @0x%06x len %d: f0=%d f1=%d byteLen=%d count=%d masks=%s"
                  % (e['key_inline'], e['value_off'], e['value_len'], bv['f0'], bv['f1'], bv['byte_len'], bv['count'], bv['masks']))
                if bv['count'] == len(car.tokens):
                    P("     per KEYFORMAT token: " + ', '.join('%s=%s' % (KEY_TOKEN_NAMES.get(tk, '?'), ('0x%x' % (m & 0xffffffff)) if m >= 0 else '-1') for tk, m in zip(car.tokens, bv['masks'])))
    P("== RENDITIONS: %d" % len(car.renditions))
    for n, r in enumerate(car.renditions):
        key, csi = r['key'], r['csi']
        P("-- rendition %d: key @0x%06x len %d = %s" % (n, r['key_off'], r['key_len'], hexline(r['key_bytes'])))
        P("     " + ', '.join('%s=%d' % (KEY_TOKEN_NAMES.get(t, '?'), key[t]) for t in car.tokens))
        o = csi['off']
        P("   value @0x%06x len %d  sha256=%s" % (o, csi['len'], r['sha256']))
        P("   [0x%06x+000] 'CTSI' version=%d flags=0x%08x width=%d height=%d scaleFactor=%d pixelFormat=%r colorSpace=0x%x (id %d)"
          % (o, csi['version'], csi['flags'], csi['width'], csi['height'], csi['scale_factor'], csi['pixel_format'], (csi['colorspace_reserved'] << 4) | csi['colorspace_id'], csi['colorspace_id']))
        P("   [0x%06x+032] modtime=%d layout=0x%x (%s) zero=%d name[128]=%r" % (o, csi['modtime'], csi['layout'], LAYOUT_NAMES.get(csi['layout'], '?'), csi['layout_zero'], csi['name']))
        P("   [0x%06x+168] tlvLength=%d unknown=%d zero=%d renditionLength=%d" % (o, csi['tlv_len'], csi['bitmaplist_unknown'], csi['bitmaplist_zero'], csi['rendition_len']))
        for t in csi['tlvs']:
            P("   [0x%06x] TLV type=0x%x (%s) len=%d data=%s %s" % (t['off'], t['type'], TLV_NAMES.get(t['type'], '?'), t['len'], t['raw'].hex(), t['decoded'] if t['decoded'] is not None else ''))
        P("   [0x%06x] payload tag %r, %d bytes" % (csi['payload_off'], csi['payload_tag'], len(csi['payload'])))
        if 'celm' in csi:
            c = csi['celm']
            P("     CELM version=%d compression=%d (%s)%s" % (c['version'], c['compression'], COMPRESSION_NAMES.get(c['compression'], '?'),
                                                              (' chunks=%d' % c['chunk_count']) if 'chunks' in c else ''))
            for i, ch in enumerate(c.get('chunks', [])):
                P("     [0x%06x] chunk %d 'CBCK' f1=%d f2=%d rows=%d compressedLen=%d data@0x%06x first bytes %s"
                  % (ch['off'], i, ch['f1'], ch['f2'], ch['rows'], ch['comp_len'], ch['data_off'], ch['data'][:12].hex()))
            for note in r['pixel_notes']:
                P("       " + note)
            if r['decode_error']:
                P("     PAYLOAD NOT DECODED: %s" % r['decode_error'])
            if r['pixels'] is not None:
                px = r['pixels']
                P("     decoded pixels: %d bytes; first 4 pixels (byte order as stored) %s; alpha all 255: %s"
                  % (len(px), hexline(px[:16]), all(px[i] == 255 for i in range(3, len(px), 4))))
        if 'msis' in csi:
            m = csi['msis']
            P("     MSIS version=%d count=%d entries=%s" % (m['version'], m['count'], m['entries']))
        if 'rawd' in csi:
            P("     RAWD %s" % csi['rawd'])
        if hexdump:
            for off in range(0, min(len(r['value']), 320), 32):
                P("     %06x: %s" % (o + off, r['value'][off:off + 32].hex()))
    P("== assetutil view")
    P(json.dumps([car.header_asset_view()] + [car.rendition_asset_view(r) for r in car.renditions], indent=1, sort_keys=True))


# ---------------------------------------------------------------- checks
IDENTITY_FIELDS = ('AssetType', 'Idiom', 'Name', 'NameIdentifier', 'RenditionName', 'Icon Index', 'Sizes', 'Scale')


def check_against(car, assetutil_path, ignore_fields=()):
    """Compare our decode with `xcrun assetutil --info` JSON. Returns (problems, notes).

    Renditions are paired by identity (AssetType, Idiom, Name, NameIdentifier, RenditionName,
    Icon Index, Sizes, Scale); every other field assetutil prints is then compared.  Pass
    ignore_fields=('SHA1Digest', 'SizeOnDisk') to check a file against the assetutil decode of a
    DIFFERENT compile of the same catalog (only the compressed payload bytes may differ)."""
    au = json.load(open(assetutil_path))
    problems = []
    notes = []
    ignore = set(ignore_fields)
    if not au or not isinstance(au, list):
        return ["assetutil.json is not a JSON array"]
    hdr = au[0]
    mine = car.header_asset_view()
    for k, v in mine.items():
        if k not in hdr:
            problems.append("header: assetutil has no %r (we derive %r)" % (k, v))
        elif hdr[k] != v:
            problems.append("header: %s: assetutil=%r file=%r" % (k, hdr[k], v))
    entries = au[1:]
    if len(entries) != len(car.renditions):
        problems.append("assetutil lists %d renditions, file has %d (CARHEADER renditionCount=%d)" % (len(entries), len(car.renditions), car.header['rendition_count']))
    unmatched_file = list(range(len(car.renditions)))
    views = [car.rendition_asset_view(r, with_pixels=False) for r in car.renditions]
    ident = lambda v: tuple(json.dumps(v.get(f), sort_keys=True) for f in IDENTITY_FIELDS)
    for ei, ent in enumerate(entries):
        cands = [i for i in unmatched_file if ident(views[i]) == ident(ent)]
        if not cands:
            problems.append("assetutil entry %d (%s) matches no rendition by identity fields %s"
                            % (ei, ', '.join('%s=%r' % (f, ent.get(f)) for f in IDENTITY_FIELDS if f in ent), IDENTITY_FIELDS))
            continue
        i = cands[0]
        unmatched_file.remove(i)
        r = car.renditions[i]
        view = car.rendition_asset_view(r)
        for k, v in view.items():
            if k in ignore:
                continue
            if k not in ent:
                problems.append("rendition %d (value @0x%06x): we derive %s=%r but assetutil has no such field" % (i, r['value_off'], k, v))
            elif ent[k] != v:
                problems.append("rendition %d (value @0x%06x): %s: assetutil=%r file=%r" % (i, r['value_off'], k, ent[k], v))
        for k in ent:
            if k in ignore:
                continue
            if k not in view:
                if k == 'Opaque' and r['pixels'] is None:
                    notes.append("rendition %d (value @0x%06x): Opaque=%r not checked (pixels not decoded%s)" % (i, r['value_off'], ent[k], (': ' + r['decode_error']) if r['decode_error'] else ''))
                    continue
                problems.append("rendition %d (value @0x%06x): assetutil field %s=%r not derived by this tool" % (i, r['value_off'], k, ent[k]))
    for i in unmatched_file:
        r = car.renditions[i]
        problems.append("rendition %d (key @0x%06x, value @0x%06x, sha256 %s...) has no assetutil entry" % (i, r['key_off'], r['value_off'], r['sha256'][:12]))
    return problems, notes


# literal values every Xcode 26.6 (17F113) golden carries; differences are NOTEs, not errors
GOLDEN_LITERALS = {
    'coreui_version': 975, 'storage_version': 17, 'schema_version': 2, 'colorspace_id': 1, 'key_semantics': 2,
    'main_version': '@(#)PROGRAM:CoreUI  PROJECT:CoreUI-975',
    'version_string': 'Xcode 26.6 (17F113) via AssetCatalogSimulatorAgent',
    'authoring_tool': '@(#)PROGRAM:CoreThemeDefinition  PROJECT:CoreThemeDefinition-653.4  [IIO-2784.5.4]',
    'tokens': [7, 13, 12, 15, 16, 9, 17, 1, 2],
}


def strict_actool(car):
    """Layout conventions observed on every golden (see carspec-SPEC.md). Returns (errors, notes)."""
    d, bom = car.d, car.bom
    E, N = [], []
    if len(d) < Bom.HEADER_PAD or set(d[32:Bom.HEADER_PAD]) != {0}:
        E.append("BOM header is not zero-padded to 0x200")
    if bom.n_ptrs != 256:
        E.append("block table has %d pointers, actool writes 256" % bom.n_ptrs)
    if bom.n_free != 0 or bom.index_tail != bytes(16):
        E.append("free list: count=%d tail=%s; actool writes count 0 followed by 16 zero bytes" % (bom.n_free, bom.index_tail.hex()))
    if bom.index_off + bom.index_len != len(d):
        E.append("indexOffset+indexLength = 0x%x, file length 0x%x; actool puts the index last" % (bom.index_off + bom.index_len, len(d)))
    if bom.index_len != 4 + 8 * bom.n_ptrs + 4 + 16:
        E.append("indexLength %d != 4+8*%d+4+16" % (bom.index_len, bom.n_ptrs))
    used = [(a, l, i) for i, (a, l) in enumerate(bom.blocks) if l]
    if [i for a, l, i in sorted(used)] != list(range(1, len(used) + 1)):
        E.append("blocks are not laid out in ascending index order 1..%d without holes" % len(used))
    prev = Bom.HEADER_PAD
    for a, l, i in sorted(used):
        if a % 16:
            E.append("block %d @0x%x is not 16-byte aligned" % (i, a))
        if a < prev:
            E.append("block %d @0x%x overlaps the previous block (ends 0x%x)" % (i, a, prev))
        elif set(d[prev:a]) - {0}:
            E.append("gap before block %d (0x%x..0x%x) is not zero-filled" % (i, prev, a))
        elif a - prev >= 16:
            E.append("gap before block %d is %d bytes; actool pads only to the next 16-byte boundary" % (i, a - prev))
        prev = a + l
    if used and sorted(used)[0][0] != Bom.HEADER_PAD:
        E.append("first block @0x%x, actool starts at 0x200" % sorted(used)[0][0])
    exp_vars = (prev + 15) & ~15
    if bom.vars_off != exp_vars:
        E.append("vars @0x%x; expected 0x%x (next 16-byte boundary after the last block)" % (bom.vars_off, exp_vars))
    exp_index = (bom.vars_end + 15) & ~15
    if bom.index_off != exp_index:
        E.append("index @0x%x; expected 0x%x (next 16-byte boundary after vars)" % (bom.index_off, exp_index))
    if bom.vars_off % 16 or bom.index_off % 16:
        E.append("vars/index not 16-byte aligned")
    order = [n for n, _, _ in bom.vars]
    if order != ['CARHEADER', 'RENDITIONS', 'FACETKEYS', 'APPEARANCEKEYS', 'KEYFORMAT', 'EXTENDED_METADATA', 'BITMAPKEYS']:
        N.append("vars order %s differs from actool's" % order)
    for name, t in car.trees.items():
        want_bs = 1024 if name == 'BITMAPKEYS' else 4096
        if t.hdr_len != 29:
            E.append("%s tree header is %d bytes, actool writes 29" % (name, t.hdr_len))
        if t.block_size != want_bs:
            E.append("%s tree blockSize=%d, actool writes %d" % (name, t.block_size, want_bs))
        if t.flag != (1 if name == 'BITMAPKEYS' else 0):
            E.append("%s tree flag byte @+20 = %d" % (name, t.flag))
        if t.tail != 0:
            E.append("%s tree u32 @+25 = %s, expected 0" % (name, t.tail))
        keys = [e['key'] for e in t.entries] if not t.inline_keys else []
        klen = max((len(k) for k in keys), default=0)
        if t.key_size != klen:
            E.append("%s tree keySize @+21 = %s, keys are %d bytes" % (name, t.key_size, klen))
        if len(t.leaves) != 1:
            E.append("%s has %d leaf blocks; actool writes one" % (name, len(t.leaves)))
        for leaf in t.leaves:
            pa, pl, cnt, fwd, bwd, ents = leaf
            if fwd or bwd:
                E.append("%s leaf forward/backward = %d/%d, expected 0/0" % (name, fwd, bwd))
            total_keys = sum(len(k) for k in keys)
            if pl != t.block_size + total_keys:
                E.append("%s leaf block is %d bytes; actool writes blockSize(%d) + total key bytes(%d) = %d" % (name, pl, t.block_size, total_keys, t.block_size + total_keys))
            copy_off, copy = t.leaf_inline_key_copy(leaf)
            expect = bytes(4) + b''.join(keys)
            if copy[:len(expect)] != expect:
                E.append("%s leaf @0x%x: bytes after the entry table are not [4 zero bytes][keys in entry order]" % (name, pa))
            if set(copy[len(expect):]) - {0}:
                E.append("%s leaf @0x%x: bytes after the inline key copy are not zero" % (name, pa))
    # block order: CARHEADER, RENDITIONS tree+leaf, FACETKEYS tree+leaf, APPEARANCEKEYS tree+leaf, appearance key/value,
    # facet key/value ..., KEYFORMAT, (key,value)*, EXTENDED_METADATA, BITMAPKEYS tree+leaf, bitmap values
    vi = bom.var_index
    try:
        seq = [vi['CARHEADER'], vi['RENDITIONS'], car.trees['RENDITIONS'].child, vi['FACETKEYS'], car.trees['FACETKEYS'].child,
               vi['APPEARANCEKEYS'], car.trees['APPEARANCEKEYS'].child]
        for a in car.appearances:
            seq += [a['key_index'], a['value_index']]
        for f in car.facets:
            seq += [f['key_index'], f['value_index']]
        seq.append(vi['KEYFORMAT'])
        rkv = sorted((r['key_index'], r['value_index']) for r in car.renditions)
        for k, v in rkv:
            seq += [k, v]
        seq += [vi['EXTENDED_METADATA'], vi['BITMAPKEYS'], car.trees['BITMAPKEYS'].child]
        seq += [b['value_index'] for b in car.bitmapkeys]
        if seq != list(range(1, len(seq) + 1)):
            N.append("block index order differs from actool's: %s" % seq)
        for k, v in rkv:
            if v != k + 1:
                N.append("rendition key block %d is not immediately followed by its value block (%d)" % (k, v))
    except KeyError as e:
        N.append("could not verify block order (missing %s)" % e)
    h = car.header
    for f in ('coreui_version', 'storage_version', 'schema_version', 'colorspace_id', 'key_semantics', 'main_version', 'version_string'):
        if h[f] != GOLDEN_LITERALS[f]:
            N.append("CARHEADER %s = %r, goldens have %r" % (f, h[f], GOLDEN_LITERALS[f]))
    if h['storage_timestamp'] != 0:
        N.append("CARHEADER storageTimestamp = %d, goldens have 0" % h['storage_timestamp'])
    if h['uuid'] != bytes(16) or h['associated_checksum'] != 0:
        N.append("CARHEADER uuid/checksum nonzero, goldens have zeros")
    if car.meta and car.meta['authoring_tool'] != GOLDEN_LITERALS['authoring_tool']:
        N.append("EXTENDED_METADATA authoringTool differs from goldens: %r" % car.meta['authoring_tool'])
    if car.tokens != GOLDEN_LITERALS['tokens']:
        N.append("KEYFORMAT tokens %s differ from goldens %s" % (car.tokens, GOLDEN_LITERALS['tokens']))
    # RENDITIONS ordering
    keys = [r['key_bytes'] for r in car.renditions]
    if keys != sorted(keys):
        N.append("RENDITIONS keys are not in ascending memcmp order (UNVERIFIED which order actool uses; goldens are consistent with memcmp and with u16-tuple order)")
    tup = [tuple(struct.unpack('<%dH' % (len(k) // 2), k)) for k in keys]
    if tup != sorted(tup):
        N.append("RENDITIONS keys are not in ascending u16-tuple order")
    for r in car.renditions:
        csi = r['csi']
        if csi['version'] != 1 or csi['bitmaplist_unknown'] != 1 or csi['bitmaplist_zero'] != 0 or csi['modtime'] != 0 or csi['layout_zero'] != 0:
            E.append("rendition value @0x%x: CSI version/unknown/zero/modtime fields differ from goldens (1,1,0,0)" % csi['off'])
        if 'celm' in csi:
            c = csi['celm']
            if c['version'] != 3 or c['compression'] != 4:
                N.append("rendition value @0x%x: CELM version=%d compression=%d, goldens have 3/4 (lzfse)" % (csi['off'], c['version'], c['compression']))
            for ch in c.get('chunks', []):
                if ch['f1'] or ch['f2']:
                    E.append("chunk @0x%x: f1/f2 = %d/%d, goldens have 0/0" % (ch['off'], ch['f1'], ch['f2']))
            if csi['height'] == 1024 and [ch['rows'] for ch in c.get('chunks', [])] != [341, 341, 341, 1]:
                N.append("rendition value @0x%x: chunk rows %s, goldens split 1024 rows as 341/341/341/1" % (csi['off'], [ch['rows'] for ch in c.get('chunks', [])]))
            types = [t['type'] for t in csi['tlvs']]
            if types != [0x3E9, 0x3EB, 0x3EC, 0x3EE, 0x3EF]:
                N.append("rendition value @0x%x: image TLV list %s differs from goldens [3e9,3eb,3ec,3ee,3ef]" % (csi['off'], [hex(t) for t in types]))
        if 'msis' in csi:
            types = [t['type'] for t in csi['tlvs']]
            if types != [0x3EC, 0x3EE]:
                N.append("rendition value @0x%x: MSIS TLV list %s differs from goldens [3ec,3ee]" % (csi['off'], [hex(t) for t in types]))
    # facet identifiers vs the name hash
    for f in car.facets:
        ident = f['attr_map'].get(17)
        if ident is not None and NAME_IDENTIFIER is not None:
            want = NAME_IDENTIFIER(f['name'])
            if want != ident:
                E.append("facet %r identifier %d != name hash %d (see carspec-SPEC.md)" % (f['name'], ident, want))
    return E, N


# NameIdentifier hash: filled in once proven on the goldens (carspec-SPEC.md section on the hash).
def name_identifier(name):
    """NameIdentifier (rendition key attribute 17 / facet identifier) as actool computes it.

    Fitted on and verified against all 56 distinct names (58 points) of the goldens
    (goldens/name-table.json + goldens/batch/hash-sweep.json); seed 100823 is the only
    value in [0, 2^30) that fits, the additive constant is 0.  Names are hashed as their
    UTF-8 bytes; only ASCII names of length 1..16 were available, so non-ASCII and
    longer names are UNVERIFIED.
    """
    v = 100823
    for c in name.encode('utf-8'):
        v = (v * 33 + c) & 0xFFFFFFFFFFFFFFFF
    h = 0
    for i in range(4):
        h = (h * 33 + ((v >> (16 * i)) & 0xFFFF)) & 0xFFFF
    return h


NAME_IDENTIFIER = name_identifier


# ---------------------------------------------------------------- main
def write_pixels(car, outdir):
    os.makedirs(outdir, exist_ok=True)
    written = []
    for n, r in enumerate(car.renditions):
        if r['pixels'] is None:
            continue
        csi = r['csi']
        base = os.path.join(outdir, "rendition%d-%s-idiom%d" % (n, os.path.splitext(csi['name'])[0], r['key'].get(15, 0)))
        open(base + '.bgra', 'wb').write(r['pixels'])
        written.append(base + '.bgra')
        try:
            from PIL import Image
            im = Image.frombytes('RGBA', (csi['width'], csi['height']), r['pixels'], 'raw', 'BGRA')
            im.save(base + '.png')
            written.append(base + '.png')
        except ImportError:
            pass
    return written


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('car')
    ap.add_argument('--check-against', metavar='assetutil.json')
    ap.add_argument('--ignore-fields', default='', metavar='F1,F2', help='assetutil fields to skip in --check-against, e.g. SHA1Digest,SizeOnDisk when the payload bytes legitimately differ')
    ap.add_argument('--strict-actool', action='store_true')
    ap.add_argument('--pixels-dir')
    ap.add_argument('--no-decode', action='store_true', help='skip LZFSE/LZVN pixel decoding')
    ap.add_argument('--require-decode', action='store_true', help='fail if any image payload cannot be decompressed')
    ap.add_argument('--quiet', action='store_true', help='no dump, only check results')
    ap.add_argument('--hexdump', action='store_true')
    args = ap.parse_args(argv)
    data = open(args.car, 'rb').read()
    try:
        car = Car(data, decode=not args.no_decode)
    except CarError as e:
        print("ERROR %s: %s" % (args.car, e))
        return 1
    except LzfseError as e:
        print("ERROR %s: LZFSE: %s" % (args.car, e))
        return 1
    if not args.quiet:
        dump(car, hexdump=args.hexdump)
    rc = 0
    if args.pixels_dir:
        for w in write_pixels(car, args.pixels_dir):
            print("wrote", w)
    if args.require_decode:
        for n, r in enumerate(car.renditions):
            if r['decode_error']:
                print("DECODE FAIL %s: rendition %d: %s" % (args.car, n, r['decode_error']))
                rc = 1
    if args.check_against:
        ignore = tuple(f for f in args.ignore_fields.split(',') if f)
        probs, notes = check_against(car, args.check_against, ignore)
        if ignore:
            print("NOTE %s: fields ignored in the assetutil comparison: %s" % (args.car, ', '.join(ignore)))
        for n in notes:
            print("NOTE %s: %s" % (args.car, n))
        if probs:
            rc = 1
            for p in probs:
                print("CHECK FAIL %s: %s" % (args.car, p))
        else:
            print("CHECK OK %s agrees with %s (%d renditions)" % (args.car, args.check_against, len(car.renditions)))
    if args.strict_actool:
        errs, notes = strict_actool(car)
        for n in notes:
            print("NOTE %s: %s" % (args.car, n))
        if errs:
            rc = 1
            for e in errs:
                print("STRICT FAIL %s: %s" % (args.car, e))
        else:
            print("STRICT OK %s follows the actool layout conventions" % args.car)
    return rc


if __name__ == '__main__':
    sys.exit(main())
