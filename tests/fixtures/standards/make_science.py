"""Writes the small medical and scientific fixtures used by crates/formats/tests/standards_science.rs.

Run from the repository root with the Python standard library only:

    python3 -I tests/fixtures/standards/make_science.py

Every value is synthetic (no real patient, sample or observation). The output is deterministic.
"""
import gzip
import math
import os
import struct
import zlib

OUT = os.path.dirname(os.path.abspath(__file__))


def write(name, data):
    with open(os.path.join(OUT, name), "wb") as f:
        f.write(data)


def gz(data):
    return gzip.compress(data, mtime=0)


def bgzf(data, size=60000):
    """BGZF: gzip members of at most 64 KiB with a BC extra field, then the end-of-file block."""
    out = b""
    for at in range(0, len(data), size):
        chunk = data[at:at + size]
        c = zlib.compressobj(6, zlib.DEFLATED, -15)
        cdata = c.compress(chunk) + c.flush()
        total = 18 + len(cdata) + 8
        out += b"\x1f\x8b\x08\x04\0\0\0\0\0\xff" + struct.pack("<H", 6) + b"BC" + struct.pack("<HH", 2, total - 1)
        out += cdata + struct.pack("<II", zlib.crc32(chunk), len(chunk))
    out += bytes.fromhex("1f8b08040000000000ff0600424302001b0003000000000000000000")
    return out


# ---------------------------------------------------------------------------------------------
# DICOM

LONG = {"OB", "OD", "OF", "OL", "OV", "OW", "SQ", "SV", "UC", "UN", "UR", "UT", "UV"}


def val(v, vr):
    if isinstance(v, str):
        v = v.encode("latin-1")
        if len(v) % 2:
            v += b"\0" if vr == "UI" else b" "
    return v


def ex(tag, vr, v, be=False):
    e = ">" if be else "<"
    v = val(v, vr)
    head = struct.pack(e + "HH", tag >> 16, tag & 0xFFFF) + vr.encode()
    if vr in LONG:
        return head + b"\0\0" + struct.pack(e + "I", len(v)) + v
    return head + struct.pack(e + "H", len(v)) + v


def im(tag, v, vr="LO"):
    v = val(v, vr)
    return struct.pack("<HHI", tag >> 16, tag & 0xFFFF, len(v)) + v


def us(*v, be=False):
    return struct.pack((">" if be else "<") + "%dH" % len(v), *v)


def item(body, be=False, undefined=False):
    e = ">" if be else "<"
    if undefined:
        return struct.pack(e + "HHI", 0xFFFE, 0xE000, 0xFFFFFFFF) + body + struct.pack(e + "HHI", 0xFFFE, 0xE00D, 0)
    return struct.pack(e + "HHI", 0xFFFE, 0xE000, len(body)) + body


def seq_end(be=False):
    return struct.pack((">" if be else "<") + "HHI", 0xFFFE, 0xE0DD, 0)


def meta(sop_class, sop_instance, syntax):
    body = (
        ex(0x00020001, "OB", b"\0\1")
        + ex(0x00020002, "UI", sop_class)
        + ex(0x00020003, "UI", sop_instance)
        + ex(0x00020010, "UI", syntax)
        + ex(0x00020012, "UI", "1.2.826.0.1.3680043.10.999")
        + ex(0x00020013, "SH", "ISOLINEAR_TEST")
    )
    return b"\0" * 128 + b"DICM" + ex(0x00020000, "UL", struct.pack("<I", len(body))) + body


def ct_explicit():
    sop = "1.2.840.10008.5.1.4.1.1.2"
    inst = "1.2.826.0.1.3680043.10.999.1.1"
    rows, cols = 6, 8
    pixels = b"".join(struct.pack("<h", (y * cols + x) * 10 - 100) for y in range(rows) for x in range(cols))
    ref_item = item(ex(0x00081150, "UI", sop) + ex(0x00081155, "UI", "1.2.826.0.1.3680043.10.999.1.0"))
    code_item = item(ex(0x00080100, "SH", "T-A0100") + ex(0x00080104, "LO", "Brain"))
    voi_item = item(
        ex(0x00283002, "US", us(4, 0, 16))
        + ex(0x00283003, "LO", "Test LUT")
        + ex(0x00283006, "US", us(0, 100, 200, 300))
        + ex(0x0040A043, "SQ", code_item),
        undefined=True,
    )
    ds = (
        ex(0x00080005, "CS", "ISO_IR 100")
        + ex(0x00080008, "CS", "ORIGINAL\\PRIMARY\\AXIAL")
        + ex(0x00080016, "UI", sop)
        + ex(0x00080018, "UI", inst)
        + ex(0x00080020, "DA", "20240115")
        + ex(0x00080030, "TM", "101530")
        + ex(0x00080050, "SH", "ACC123")
        + ex(0x00080060, "CS", "CT")
        + ex(0x00080070, "LO", "Example Medical")
        + ex(0x00080080, "LO", "Example Hospital")
        + ex(0x00080090, "PN", "Smith^Anna")
        + ex(0x00081030, "LO", "CT HEAD")
        + ex(0x0008103E, "LO", "Axial 5mm")
        + ex(0x00081090, "LO", "Scanner 3000")
        + ex(0x00081140, "SQ", ref_item)
        + ex(0x00100010, "PN", "Doe^Jane")
        + ex(0x00100020, "LO", "PID-0042")
        + ex(0x00100030, "DA", "19700101")
        + ex(0x00100040, "CS", "F")
        + ex(0x00101010, "AS", "054Y")
        + ex(0x00180050, "DS", "5.0")
        + ex(0x00180060, "DS", "120")
        + ex(0x0020000D, "UI", "1.2.826.0.1.3680043.10.999.1")
        + ex(0x0020000E, "UI", "1.2.826.0.1.3680043.10.999.1.2")
        + ex(0x00200011, "IS", "3")
        + ex(0x00200013, "IS", "7")
        + ex(0x00200032, "DS", "-120\\-120\\50")
        + ex(0x00200037, "DS", "1\\0\\0\\0\\1\\0")
        + ex(0x00200052, "UI", "1.2.826.0.1.3680043.10.999.1.3")
        + ex(0x00280002, "US", us(1))
        + ex(0x00280004, "CS", "MONOCHROME2")
        + ex(0x00280010, "US", us(rows))
        + ex(0x00280011, "US", us(cols))
        + ex(0x00280030, "DS", "0.5\\0.5")
        + ex(0x00280100, "US", us(16))
        + ex(0x00280101, "US", us(12))
        + ex(0x00280102, "US", us(11))
        + ex(0x00280103, "US", us(1))
        + ex(0x00281050, "DS", "40")
        + ex(0x00281051, "DS", "400")
        + ex(0x00281052, "DS", "-1024")
        + ex(0x00281053, "DS", "1")
        + ex(0x00281054, "LO", "HU")
        + struct.pack("<HH", 0x0028, 0x3010) + b"SQ\0\0" + struct.pack("<I", 0xFFFFFFFF) + voi_item + seq_end()
        + ex(0x7FE00010, "OW", pixels)
    )
    write("ct_explicit.dcm", meta(sop, inst, "1.2.840.10008.1.2.1") + ds)


def mr_implicit():
    sop = "1.2.840.10008.5.1.4.1.1.4"
    inst = "1.2.826.0.1.3680043.10.999.2.1"
    pixels = bytes(range(16)) + bytes(range(100, 116))
    ds = (
        im(0x00080016, sop, "UI")
        + im(0x00080018, inst, "UI")
        + im(0x00080060, "MR", "CS")
        + im(0x00090010, "ISOLINEAR")
        + im(0x00091001, b"\1\2\3\4")
        + im(0x00100010, "Roe^Richard", "PN")
        + im(0x00180080, "2000", "DS")
        + im(0x00180087, "1.5", "DS")
        + im(0x00280002, us(1))
        + im(0x00280004, "MONOCHROME1", "CS")
        + im(0x00280008, "2", "IS")
        + im(0x00280010, us(4))
        + im(0x00280011, us(4))
        + im(0x00280100, us(8))
        + im(0x00280101, us(8))
        + im(0x00280102, us(7))
        + im(0x00280103, us(0))
        + im(0x7FE00010, pixels)
    )
    write("mr_implicit.dcm", meta(sop, inst, "1.2.840.10008.1.2") + ds)


def rgb_big_endian():
    sop = "1.2.840.10008.5.1.4.1.1.7"
    inst = "1.2.826.0.1.3680043.10.999.3.1"
    pixels = bytes([255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 128, 128, 128])
    be = True
    ds = (
        ex(0x00080016, "UI", sop, be)
        + ex(0x00080018, "UI", inst, be)
        + ex(0x00080060, "CS", "OT", be)
        + ex(0x00280002, "US", us(3, be=be), be)
        + ex(0x00280004, "CS", "RGB", be)
        + ex(0x00280006, "US", us(0, be=be), be)
        + ex(0x00280010, "US", us(2, be=be), be)
        + ex(0x00280011, "US", us(3, be=be), be)
        + ex(0x00280100, "US", us(8, be=be), be)
        + ex(0x00280101, "US", us(8, be=be), be)
        + ex(0x00280102, "US", us(7, be=be), be)
        + ex(0x00280103, "US", us(0, be=be), be)
        + ex(0x7FE00010, "OB", pixels, be)
    )
    write("rgb_big_endian.dcm", meta(sop, inst, "1.2.840.10008.1.2.2") + ds)


def jpeg_encapsulated():
    sop = "1.2.840.10008.5.1.4.1.1.7"
    inst = "1.2.826.0.1.3680043.10.999.4.1"
    frag = b"\xff\xd8\xff\xe0\0\x10JFIF\0\xff\xd9\0"
    ds = (
        ex(0x00080016, "UI", sop)
        + ex(0x00080018, "UI", "1.2.826.0.1.3680043.10.999.4.2")
        + ex(0x00280002, "US", us(1))
        + ex(0x00280004, "CS", "MONOCHROME2")
        + ex(0x00280010, "US", us(16))
        + ex(0x00280011, "US", us(16))
        + ex(0x00280100, "US", us(8))
        + struct.pack("<HH", 0x7FE0, 0x0010) + b"OB\0\0" + struct.pack("<I", 0xFFFFFFFF)
        + item(b"") + item(frag) + seq_end()
    )
    # The SOP instance in the meta header differs from the data set's: a failed rule.
    write("jpeg_encapsulated.dcm", meta(sop, inst, "1.2.840.10008.1.2.4.50") + ds)


def bare_dataset():
    ds = (
        im(0x00080016, "1.2.840.10008.5.1.4.1.1.7", "UI")
        + im(0x00080018, "1.2.826.0.1.3680043.10.999.5.1", "UI")
        + im(0x00080060, "OT", "CS")
        + im(0x00100010, "Bare^Data", "PN")
        + im(0x00100020, "B-1")
    )
    write("bare_dataset.dcm", ds)


# ---------------------------------------------------------------------------------------------
# EDF+ and BDF


def edf_header(version, patient, recording, date, time, reserved, records, duration, signals):
    ns = len(signals)
    h = version.ljust(8).encode("latin-1") if isinstance(version, str) else version
    h += patient.ljust(80).encode() + recording.ljust(80).encode()
    h += date.encode() + time.encode() + str(256 * (ns + 1)).ljust(8).encode()
    h += reserved.ljust(44).encode() + str(records).ljust(8).encode() + str(duration).ljust(8).encode()
    h += str(ns).ljust(4).encode()
    for i, w in [(0, 16), (1, 80), (2, 8), (3, 8), (4, 8), (5, 8), (6, 8), (7, 80), (8, 8), (9, 32)]:
        h += b"".join(str(s[i]).ljust(w).encode() for s in signals)
    assert len(h) == 256 * (ns + 1)
    return h


def edf_plus():
    signals = [
        ("EEG Fpz-Cz", "AgAgCl electrode", "uV", -500, 500, -32768, 32767, "HP:0.1Hz LP:75Hz", 100, ""),
        ("Temp", "Thermistor", "degC", 30, 40, -2048, 2047, "", 10, ""),
        ("EDF Annotations", "", "", -1, 1, -32768, 32767, "", 30, ""),
    ]
    h = edf_header(
        "0",
        "MCH-0234567 F 02-MAY-1951 Haagse_Harry",
        "Startdate 02-MAR-2002 EMG561 BK/JOP Sony.",
        "02.03.02",
        "14.30.00",
        "EDF+C",
        3,
        1,
        signals,
    )
    data = b""
    for k in range(3):
        data += b"".join(struct.pack("<h", int(10000 * math.sin(2 * math.pi * 5 * (k + n / 100)))) for n in range(100))
        data += b"".join(struct.pack("<h", -2048 if n % 2 == 0 else 2047) for n in range(10))
        tal = b"+%d\x14\x14\x00" % k
        if k == 1:
            tal += b"+1.5\x150.5\x14Arousal\x14\x00"
        if k == 2:
            tal += b"+2.25\x14Lights off\x14Snoring\x14\x00"
        data += tal.ljust(60, b"\0")
    write("sample.edf", h + data)


def bdf():
    signals = [
        ("A1", "Active electrode", "uV", -262144, 262143, -8388608, 8388607, "HP:DC", 8, ""),
        ("Status", "Triggers", "Boolean", -8388608, 8388607, -8388608, 8388607, "No filtering", 8, ""),
    ]
    h = edf_header(b"\xffBIOSEMI", "Anonymous", "Test recording", "15.01.24", "09.00.00", "24BIT", 2, 1, signals)

    def s24(v):
        return struct.pack("<i", v)[:3]

    data = b""
    for k in range(2):
        data += b"".join(s24(-8388608 if n == 0 else 8388607 if n == 1 else n * 1000) for n in range(8))
        data += b"".join(s24(k) for _ in range(8))
    write("sample.bdf", h + data)


# ---------------------------------------------------------------------------------------------
# VCF, FASTA, FASTQ

VCF = """##fileformat=VCFv4.2
##fileDate=20240115
##source=isolinear-test
##reference=GRCh38
##contig=<ID=chr1,length=248956422>
##contig=<ID=chr2,length=242193529>
##INFO=<ID=DP,Number=1,Type=Integer,Description="Total depth">
##INFO=<ID=AF,Number=A,Type=Float,Description="Allele frequency, per ALT allele">
##INFO=<ID=SVTYPE,Number=1,Type=String,Description="Type of structural variant">
##FORMAT=<ID=GT,Number=1,Type=String,Description="Genotype">
##FORMAT=<ID=DP,Number=1,Type=Integer,Description="Read depth">
##FILTER=<ID=q10,Description="Quality below 10">
#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\tFORMAT\tS1\tS2\tS3
chr1\t1000\trs1\tA\tG\t50\tPASS\tDP=20;AF=0.5\tGT:DP\t0/1:10\t1/1:8\t0/0:12
chr1\t2000\t.\tAT\tA\t30\tPASS\tDP=15\tGT:DP\t0/1:5\t0/0:6\t./.:0
chr1\t3000\trs3\tC\tT,G\t20\tq10\tDP=9;AF=0.2,0.1\tGT\t1/2\t0/1\t0/0
chr2\t500\t.\tG\tGTT\t99\tPASS\tDP=30\tGT\t0|1\t1|1\t0|0
chr2\t800\t.\tC\t<DEL>\t10\tPASS\tSVTYPE=DEL;END=900\tGT\t0/1\t0/0\t0/0
""".replace("\\t", "\t")


def fasta():
    seqs = [
        ("seq1", "Example contig one", "GC" * 25 + "AT" * 25),
        ("seq2", "second contig", "ACGTTGCA" * 10),
        ("seq3", "", "AAAATTTTGGGGCCCCNNNNAAAATTTTGGGGCCCCNNNNAAAATTTTGG"[:60]),
        ("seq4", "short", "ACGT" * 10),
        ("seq5", "", "GGGGGCCCCCAAAAATTTTT"),
    ]
    text = ""
    for name, desc, s in seqs:
        text += ">" + name + (" " + desc if desc else "") + "\n"
        for at in range(0, len(s), 60):
            text += s[at:at + 60] + "\n"
    write("sample.fa", text.encode())
    write("sample.fa.gz", gz(text.encode()))
    protein = (
        ">sp|P69905|HBA_TEST Haemoglobin-like test protein\n"
        "MVLSPADKTNVKAAWGKVGAHAGEYGAEALERMFLSFPTTKTYFPHFDLSHGSAQVKGHG\n"
        "KKVADALTNAVAHVDDMPNALSALSDLHAHKL\n"
        ">test2 Small test peptide\nMKWVTFISLLFLFSSAYS\n"
    )
    write("protein.faa", protein.encode())


def fastq():
    reads = [
        ("r1 1:N:0", "ACGTACGTAC", "IIIIIIIIII"),
        ("r2 1:N:0", "GGGGCCCCAA", "IIIII#####"),
        ("r3 1:N:0", "ACGTN", "5555!"),
        ("r4 1:N:0", "TTTTTTTTTTTT", "IIIIIIIIIIII"),
        ("r5 1:N:0", "ACGTAC", "??????"),
    ]
    text = "".join("@%s\n%s\n+\n%s\n" % r for r in reads)
    write("sample.fastq", text.encode())
    write("sample.fq.gz", gz(text.encode()))
    old = "@s1\nACGTA\n+s1\nhhhhh\n@s2\nACG\n+\nh@B\n"
    write("illumina64.fq", old.encode())


# ---------------------------------------------------------------------------------------------
# BAM and CRAM

SAM_HEADER = (
    "@HD\tVN:1.6\tSO:coordinate\n"
    "@SQ\tSN:chr1\tLN:1000\tM5:0123456789abcdef0123456789abcdef\tUR:file:ref.fa\n"
    "@SQ\tSN:chr2\tLN:500\n"
    "@RG\tID:rg1\tSM:sampleA\tPL:ILLUMINA\n"
    "@PG\tID:bwa\tPN:bwa\tVN:0.7.17\tCL:bwa mem ref.fa r1.fq r2.fq\n"
    "@CO\tsynthetic test data\n"
)


def bam_record(name, flag, ref, pos, mapq, cigar, seq, next_ref, next_pos, tlen):
    ops = b"".join(struct.pack("<I", n << 4 | "MIDNSHP=X".index(op)) for n, op in cigar)
    codes = [("=ACMGRSVTWYHKDBN".index(c)) for c in seq]
    if len(codes) % 2:
        codes.append(0)
    packed = bytes(codes[i] << 4 | codes[i + 1] for i in range(0, len(codes), 2))
    body = struct.pack(
        "<iiBBHHHIiii", ref, pos, len(name) + 1, mapq, 4680, len(cigar), flag, len(seq), next_ref, next_pos, tlen
    )
    body += name.encode() + b"\0" + ops + packed + bytes([30] * len(seq)) + b"NMC\x01"
    return struct.pack("<i", len(body)) + body


def bam():
    text = SAM_HEADER.encode()
    data = b"BAM\1" + struct.pack("<i", len(text)) + text + struct.pack("<i", 2)
    for name, length in [("chr1", 1000), ("chr2", 500)]:
        data += struct.pack("<i", len(name) + 1) + name.encode() + b"\0" + struct.pack("<i", length)
    data += bam_record("read1", 99, 0, 99, 60, [(10, "M")], "ACGTACGTAC", 0, 199, 110)
    data += bam_record("read1", 147, 0, 199, 60, [(5, "M"), (1, "I"), (4, "M")], "ACGTAACGTA", 0, 99, -110)
    data += bam_record("read2", 16, 1, 9, 30, [(3, "S"), (7, "M")], "TTTACGTACG", -1, -1, 0)
    data += bam_record("read3", 4, -1, -1, 0, [], "ACGTACGT", -1, -1, 0)
    # Small blocks so that records cross block boundaries.
    write("sample.bam", bgzf(data, size=150))


def itf8(v):
    v &= 0xFFFFFFFF
    if v < 0x80:
        return bytes([v])
    if v < 0x4000:
        return bytes([0x80 | v >> 8, v & 0xFF])
    if v < 0x200000:
        return bytes([0xC0 | v >> 16, v >> 8 & 0xFF, v & 0xFF])
    if v < 0x10000000:
        return bytes([0xE0 | v >> 24, v >> 16 & 0xFF, v >> 8 & 0xFF, v & 0xFF])
    return bytes([0xF0 | v >> 28, v >> 20 & 0xFF, v >> 12 & 0xFF, v >> 4 & 0xFF, v & 0x0F])


def container(length, ref, start, span, records, bases, blocks, landmarks):
    h = struct.pack("<i", length) + itf8(ref) + itf8(start) + itf8(span) + itf8(records)
    h += bytes([0]) + bytes([bases]) + itf8(blocks) + itf8(len(landmarks)) + b"".join(itf8(x) for x in landmarks)
    return h + struct.pack("<I", zlib.crc32(h))


def cram():
    raw = struct.pack("<i", len(SAM_HEADER)) + SAM_HEADER.encode()
    comp = gz(raw)
    block = bytes([1, 0]) + itf8(0) + itf8(len(comp)) + itf8(len(raw)) + comp
    block += struct.pack("<I", zlib.crc32(block))
    out = b"CRAM" + bytes([3, 0]) + b"isolinear-test".ljust(20, b"\0")
    out += container(len(block), 0, 0, 0, 0, 0, 1, []) + block
    payload = bytes(40)
    out += container(len(payload), 0, 100, 150, 3, 30, 2, [0]) + payload
    out += bytes.fromhex("0f000000ffffffff0fe0454f46000000000100" "05bdd94f" "00010006060100010001" "00ee63014b")
    write("sample.cram", out)


# ---------------------------------------------------------------------------------------------
# FITS


def card(key, value=None, comment=""):
    if value is None:
        s = key.ljust(8) + comment
    else:
        if isinstance(value, bool):
            v = ("T" if value else "F").rjust(20)
        elif isinstance(value, (int, float)):
            v = str(value).rjust(20)
        else:
            v = ("'" + value.replace("'", "''").ljust(8) + "'").ljust(20)
        s = key.ljust(8) + "= " + v + (" / " + comment if comment else "")
    return s.ljust(80)[:80].encode()


def block(data, fill=b"\0"):
    return data + fill * (-len(data) % 2880)


def header(cards):
    return block(b"".join(cards) + "END".ljust(80).encode(), b" ")


def fits():
    w, h = 8, 6
    out = header([
        card("SIMPLE", True, "conforms to FITS"),
        card("BITPIX", 16),
        card("NAXIS", 2),
        card("NAXIS1", w),
        card("NAXIS2", h),
        card("EXTEND", True),
        card("BZERO", 32768),
        card("BSCALE", 1),
        card("OBJECT", "M31", "target"),
        card("TELESCOP", "Isolinear Test"),
        card("OBSERVER", "Ann Example"),
        card("DATE-OBS", "2024-01-15T22:10:00"),
        card("EXPTIME", 30.0, "seconds"),
        card("BUNIT", "ADU"),
        card("COMMENT", None, "  synthetic test image"),
    ])
    out += block(b"".join(struct.pack(">h", (y * w + x) * 1000 - 32768) for y in range(h) for x in range(w)))
    out += header([
        card("XTENSION", "IMAGE"),
        card("BITPIX", -32),
        card("NAXIS", 3),
        card("NAXIS1", 4),
        card("NAXIS2", 3),
        card("NAXIS3", 2),
        card("PCOUNT", 0),
        card("GCOUNT", 1),
        card("EXTNAME", "CUBE"),
    ])
    out += block(b"".join(struct.pack(">f", k * 0.5) for k in range(12)) + b"".join(struct.pack(">f", 1000 + k) for k in range(12)))
    rows = [(1, 1.5, "alpha", (1.0, 2.0, 3.0), True), (2, -0.25, "beta", (4.0, 5.0, 6.0), False), (3, 1e3, "gamma", (7.0, 8.0, 9.0), True)]
    out += header([
        card("XTENSION", "BINTABLE"),
        card("BITPIX", 8),
        card("NAXIS", 2),
        card("NAXIS1", 41),
        card("NAXIS2", len(rows)),
        card("PCOUNT", 0),
        card("GCOUNT", 1),
        card("TFIELDS", 5),
        card("TTYPE1", "ID"),
        card("TFORM1", "J"),
        card("TTYPE2", "FLUX"),
        card("TFORM2", "E"),
        card("TUNIT2", "Jy"),
        card("TTYPE3", "NAME"),
        card("TFORM3", "8A"),
        card("TTYPE4", "VEC"),
        card("TFORM4", "3D"),
        card("TTYPE5", "FLAG"),
        card("TFORM5", "L"),
        card("EXTNAME", "CATALOG"),
    ])
    out += block(b"".join(
        struct.pack(">if", i, f) + n.encode().ljust(8) + struct.pack(">3d", *v) + (b"T" if fl else b"F")
        for i, f, n, v, fl in rows
    ))
    out += header([
        card("XTENSION", "TABLE"),
        card("BITPIX", 8),
        card("NAXIS", 2),
        card("NAXIS1", 20),
        card("NAXIS2", 2),
        card("PCOUNT", 0),
        card("GCOUNT", 1),
        card("TFIELDS", 2),
        card("TTYPE1", "STAR"),
        card("TBCOL1", 1),
        card("TFORM1", "A10"),
        card("TTYPE2", "MAG"),
        card("TBCOL2", 12),
        card("TFORM2", "F8.3"),
        card("EXTNAME", "ASCII"),
    ])
    stars = [("Vega", 0.03), ("Sirius", -1.46)]
    out += block(b"".join((n.ljust(10) + " " + "%8.3f" % m + " ").encode() for n, m in stars), b" ")
    write("sample.fits", out)


# ---------------------------------------------------------------------------------------------
# MATLAB


def el(t, data):
    b = struct.pack("<II", t, len(data)) + data
    return b + b"\0" * (-len(b) % 8)


def small(t, data):
    return struct.pack("<HH", t, len(data)) + data.ljust(4, b"\0")


def matrix(name, cls, dims, body, flags=0, nzmax=0):
    n = name.encode()
    head = el(6, struct.pack("<II", cls | flags, nzmax)) + el(5, struct.pack("<%di" % len(dims), *dims))
    head += small(1, n) if 0 < len(n) <= 4 else el(1, n)
    return el(14, head + body)


def doubles(*v):
    return el(9, struct.pack("<%dd" % len(v), *v))


def chars(s):
    return el(4, s.encode("utf-16-le"))


def mat5():
    text = b"MATLAB 5.0 MAT-file, Platform: GLNXA64, Created on: Mon Jan 15 10:00:00 2024"
    out = text.ljust(116, b" ") + b"\0" * 8 + struct.pack("<H", 0x0100) + b"IM"
    out += matrix("a", 6, [2, 3], doubles(1, 4, 2, 5, 3, 6))
    out += matrix("z", 6, [1, 2], doubles(1, 2) + doubles(0.5, -1), flags=0x800)
    names = b"x".ljust(32, b"\0") + b"label".ljust(32, b"\0")
    out += matrix("s", 2, [1, 1], small(5, struct.pack("<i", 32)) + el(1, names)
                  + matrix("", 6, [1, 1], doubles(42)) + matrix("", 4, [1, 5], chars("hello")))
    out += matrix("c", 1, [1, 2], matrix("", 8, [1, 3], el(1, bytes([1, 2, 253])))
                  + matrix("", 4, [1, 2], chars("ok")))
    out += matrix("flags", 9, [1, 3], el(2, bytes([1, 0, 1])), flags=0x200)
    out += matrix("sp", 5, [3, 3], el(5, struct.pack("<2i", 0, 2)) + el(5, struct.pack("<4i", 0, 1, 1, 2)) + doubles(7, 9), nzmax=2)
    big = matrix("big", 11, [1, 5], el(4, struct.pack("<5H", 10, 20, 30, 40, 50)))
    z = zlib.compress(big)
    out += struct.pack("<II", 15, len(z)) + z
    out += matrix("fh", 16, [1, 1], el(1, b"opaque"))
    write("level5.mat", out)


def mat4():
    out = b""
    x = struct.pack("<5i", 0, 2, 2, 0, 2) + b"x\0" + struct.pack("<4d", 1, 3, 2, 4)
    t = struct.pack("<5i", 1, 1, 5, 0, 2) + b"t\0" + struct.pack("<5d", *map(ord, "hello"))
    out = x + t
    write("level4.mat", out)


def mat73():
    text = b"MATLAB 7.3 MAT-file, Platform: GLNXA64, Created on: Mon Jan 15 10:00:00 2024 HDF5 schema 1.00 ."
    head = text.ljust(116, b" ") + b"\0" * 8 + struct.pack("<H", 0x0200) + b"IM"
    write("v73.mat", head.ljust(512, b"\0") + b"\x89HDF\r\n\x1a\n" + bytes(56))


if __name__ == "__main__":
    ct_explicit()
    mr_implicit()
    rgb_big_endian()
    jpeg_encapsulated()
    bare_dataset()
    edf_plus()
    bdf()
    write("sample.vcf", VCF.encode())
    write("sample.vcf.gz", bgzf(VCF.encode(), size=400))
    fasta()
    fastq()
    bam()
    cram()
    fits()
    mat5()
    mat4()
    mat73()
