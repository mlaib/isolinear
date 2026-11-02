"""Writes the document, mail and container fixtures of crates/formats/tests/standards_documents.rs.

Standard library only; the output is deterministic. Run from the repository root:

    python3 -I tests/fixtures/standards/make_documents.py

Binary formats (Compound File, BIFF8, PowerPoint records, PDF, DER) are assembled by hand
here from their published specifications; the content is invented.
"""
import base64
import io
import os
import struct
import sys
import zipfile
import zlib

OUT = os.path.dirname(os.path.abspath(__file__))
STAMP = (2026, 11, 1, 10, 0, 0)


def write(name, data):
    if isinstance(data, str):
        data = data.encode("utf-8")
    with open(os.path.join(OUT, name), "wb") as f:
        f.write(data)


def png(width, height, rgb):
    """A small solid-colour PNG."""
    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))
    raw = b"".join(b"\0" + bytes(rgb) * width for _ in range(height))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


PNG_RED = png(4, 3, (200, 30, 30))
PNG_BLUE = png(2, 2, (30, 30, 200))


def b64lines(data):
    s = base64.b64encode(data).decode()
    return "\r\n".join(s[i:i + 76] for i in range(0, len(s), 76))


# ----------------------------------------------------------------------------------------------
# Mail

def eml():
    html = "<html><head><style>p{color:red}</style></head><body><p>Hello Ann,</p><p>The report &amp; the invoice are attached.<img src=\"https://tracker.example/p.gif\"></p></body></html>"
    lines = [
        "Return-Path: <bounce@mail.example.org>",
        "Received: from mx.example.org (mx.example.org [192.0.2.10])",
        "\tby inbound.example.net with ESMTPS id 4abc; Sun, 1 Nov 2026 10:15:02 +0100",
        "Received: from laptop.local by mx.example.org with ESMTPSA; Sun, 1 Nov 2026 10:15:00 +0100",
        "Authentication-Results: inbound.example.net; spf=pass smtp.mailfrom=example.org; dkim=pass header.d=example.org; dmarc=pass",
        "DKIM-Signature: v=1; a=rsa-sha256; d=example.org; s=sel1; h=from:to:subject; bh=abc=; b=def=",
        "From: =?utf-8?Q?Jos=C3=A9_Garc=C3=ADa?= <jose@example.org>",
        "To: \"Doe, Ann\" <ann@example.net>, bob@example.net",
        "Cc: team@example.net",
        "Subject: =?utf-8?B?UmFwcG9ydCBtZW5zdWVs?= =?utf-8?Q?_et_facture_=E2=82=AC?=",
        "Date: Sun, 1 Nov 2026 10:14:55 +0100",
        "Message-ID: <20261101101455.42@example.org>",
        "In-Reply-To: <earlier@example.net>",
        "References: <first@example.net> <earlier@example.net>",
        "MIME-Version: 1.0",
        "X-Mailer: Example Mail 3.1",
        "Content-Type: multipart/mixed; boundary=\"outer\"",
        "",
        "This is a multi-part message in MIME format.",
        "--outer",
        "Content-Type: multipart/alternative; boundary=\"alt\"",
        "",
        "--alt",
        "Content-Type: text/plain; charset=utf-8",
        "Content-Transfer-Encoding: quoted-printable",
        "",
        "Hello Ann,",
        "",
        "The report & the invoice are attached. Total: 1 234,50 =E2=82=AC. This line is long a=",
        "nd soft-wrapped.",
        "--alt",
        "Content-Type: text/html; charset=utf-8",
        "Content-Transfer-Encoding: base64",
        "",
        b64lines(html.encode()),
        "--alt--",
        "--outer",
        "Content-Type: image/png",
        "Content-Transfer-Encoding: base64",
        "Content-ID: <logo@example.org>",
        "Content-Disposition: inline; filename=\"logo.png\"",
        "",
        b64lines(PNG_RED),
        "--outer",
        "Content-Type: application/pdf",
        "Content-Transfer-Encoding: base64",
        "Content-Disposition: attachment; filename*=utf-8''facture%20n%C2%B07.pdf",
        "",
        b64lines(b"%PDF-1.4\n% not a real document\n%%EOF\n"),
        "--outer--",
        "",
    ]
    write("message.eml", "\r\n".join(lines))


def mbox():
    msgs = [
        ("From ann@example.net Sat Oct 31 09:00:00 2026", [
            "From: Ann Doe <ann@example.net>", "To: jose@example.org", "Subject: Planning",
            "Date: Sat, 31 Oct 2026 09:00:00 +0000", "Message-ID: <a1@example.net>", "",
            "Shall we meet on Monday?", ">From the minutes: nothing new.", ""]),
        ("From jose@example.org Sun Nov  1 10:00:00 2026", [
            "From: =?utf-8?Q?Jos=C3=A9?= <jose@example.org>", "To: ann@example.net",
            "Subject: Re: Planning", "Date: Sun, 1 Nov 2026 10:00:00 +0100",
            "MIME-Version: 1.0", "Content-Type: multipart/mixed; boundary=\"b\"", "",
            "--b", "Content-Type: text/plain", "", "Yes, see the agenda.", "--b",
            "Content-Type: text/csv; name=\"agenda.csv\"", "Content-Disposition: attachment; filename=\"agenda.csv\"",
            "", "time,topic", "10:00,budget", "--b--", ""]),
        ("From ann@example.net Mon Nov  2 08:30:00 2026", [
            "From: Ann Doe <ann@example.net>", "To: jose@example.org", "Subject: Thanks",
            "Date: Mon, 2 Nov 2026 08:30:00 +0000", "", "See you.", ""]),
    ]
    out = []
    for sep, body in msgs:
        out.append(sep)
        out.extend(body)
    write("mailbox.mbox", "\n".join(out) + "\n")


# ----------------------------------------------------------------------------------------------
# Compound File Binary writer (version 3, 512-byte sectors, mini stream below 4096 bytes)

END, FREE, FATSECT, NOSTREAM = 0xFFFFFFFE, 0xFFFFFFFF, 0xFFFFFFFD, 0xFFFFFFFF


class Node:
    def __init__(self, name, data=None, children=None):
        self.name = name
        self.data = data
        self.children = children or []

    @property
    def stream(self):
        return self.data is not None


def cfb(root_children):
    root = Node("Root Entry", children=root_children)
    entries = []

    def flatten(node):
        node.index = len(entries)
        entries.append(node)
        for c in node.children:
            flatten(c)
    flatten(root)
    sectors, fat = [], []

    def alloc(data):
        if not data:
            return END
        start = len(sectors)
        n = (len(data) + 511) // 512
        for i in range(n):
            sectors.append(data[i * 512:(i + 1) * 512].ljust(512, b"\0"))
            fat.append(start + i + 1 if i + 1 < n else END)
        return start
    mini, minifat = bytearray(), []
    for e in entries:
        if not e.stream:
            continue
        if len(e.data) < 4096:
            if not e.data:
                e.start = END
                continue
            e.start = len(mini) // 64
            n = (len(e.data) + 63) // 64
            for i in range(n):
                minifat.append(e.start + i + 1 if i + 1 < n else END)
            mini += e.data.ljust(n * 64, b"\0")
        else:
            e.start = alloc(e.data)
    root.start = alloc(bytes(mini))
    root.size = len(mini)
    minifat_start = alloc(b"".join(struct.pack("<I", v) for v in minifat))

    def tree(children):
        """A balanced binary tree of the children, sorted as CFB compares names."""
        kids = sorted(children, key=lambda c: (len(c.name), c.name.upper()))

        def build(lo, hi):
            if lo >= hi:
                return NOSTREAM
            mid = (lo + hi) // 2
            n = kids[mid]
            n.left, n.right = build(lo, mid), build(mid + 1, hi)
            return n.index
        return build(0, len(kids))
    for e in entries:
        e.child = tree(e.children) if e.children else NOSTREAM
        if not hasattr(e, "left"):
            e.left = e.right = NOSTREAM
    dirbytes = bytearray()
    for e in entries:
        name = e.name.encode("utf-16-le")
        kind = 5 if e is root else (2 if e.stream else 1)
        size = root.size if e is root else (len(e.data) if e.stream else 0)
        start = getattr(e, "start", 0) if (e.stream or e is root) else 0
        rec = name.ljust(64, b"\0") + struct.pack("<HBB", len(name) + 2, kind, 1)
        rec += struct.pack("<III", e.left, e.right, e.child) + b"\0" * 16 + b"\0" * 4 + b"\0" * 16
        rec += struct.pack("<IQ", start, size)
        assert len(rec) == 128
        dirbytes += rec
    while len(dirbytes) % 512:
        dirbytes += (b"\0" * 64 + struct.pack("<HBB", 0, 0, 0) + struct.pack("<III", NOSTREAM, NOSTREAM, NOSTREAM)
                     + b"\0" * 48)
    dir_start = alloc(bytes(dirbytes))
    # FAT sectors: enough to cover themselves.
    nfat = 1
    while (len(sectors) + nfat) > nfat * 128:
        nfat += 1
    fat_start = len(sectors)
    for i in range(nfat):
        fat.append(FATSECT)
    fatbytes = b"".join(struct.pack("<I", v) for v in fat).ljust(nfat * 512, b"\xff")
    for i in range(nfat):
        sectors.append(fatbytes[i * 512:(i + 1) * 512])
    assert nfat <= 109
    header = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1" + b"\0" * 16
    header += struct.pack("<HHHHH", 0x3E, 3, 0xFFFE, 9, 6) + b"\0" * 6
    header += struct.pack("<IIIIIIIII", 0, nfat, dir_start, 0, 4096, minifat_start, (len(minifat) * 4 + 511) // 512, END, 0)
    difat = [fat_start + i for i in range(nfat)] + [FREE] * (109 - nfat)
    header += b"".join(struct.pack("<I", v) for v in difat)
    assert len(header) == 512
    return header + b"".join(sectors)


# ----------------------------------------------------------------------------------------------
# Outlook .msg

def msg():
    def u(s):
        return s.encode("utf-16-le") + b"\0\0"

    def substg(pid, ptype, data):
        return Node("__substg1.0_%04X%04X" % (pid, ptype), data)

    def props(header, fixed):
        body = header
        for pid, ptype, value in fixed:
            body += struct.pack("<II", (pid << 16) | ptype, 6) + value
        return body
    submit = (1_793_524_455 + 11_644_473_600) * 10_000_000  # 2026-11-01 09:14:15 UTC
    headers = ("Received: from mx.example.org by mail.example.net; Sun, 1 Nov 2026 10:14:20 +0100\r\n"
               "Received: from laptop by mx.example.org; Sun, 1 Nov 2026 10:14:16 +0100\r\n"
               "Authentication-Results: mail.example.net; spf=pass; dkim=pass\r\n"
               "Message-ID: <msg-1@example.org>\r\n\r\n")
    recip1 = Node("__recip_version1.0_#00000000", children=[
        substg(0x3001, 0x001F, u("Ann Doe")), substg(0x3003, 0x001F, u("ann@example.net")),
        substg(0x3002, 0x001F, u("SMTP")),
        Node("__properties_version1.0", props(b"\0" * 8, [(0x0C15, 0x0003, struct.pack("<Q", 1))])),
    ])
    recip2 = Node("__recip_version1.0_#00000001", children=[
        substg(0x3001, 0x001F, u("Bob Smith")), substg(0x39FE, 0x001F, u("bob@example.net")),
        Node("__properties_version1.0", props(b"\0" * 8, [(0x0C15, 0x0003, struct.pack("<Q", 2))])),
    ])
    big = bytes(range(256)) * 20  # 5120 bytes: stored in regular sectors
    attach1 = Node("__attach_version1.0_#00000000", children=[
        substg(0x3707, 0x001F, u("chart.png")), substg(0x370E, 0x001F, u("image/png")),
        substg(0x3701, 0x0102, PNG_BLUE),
        Node("__properties_version1.0", props(b"\0" * 8, [(0x3705, 0x0003, struct.pack("<Q", 1)),
                                                          (0x0E20, 0x0003, struct.pack("<Q", len(PNG_BLUE)))])),
    ])
    attach2 = Node("__attach_version1.0_#00000001", children=[
        substg(0x3707, 0x001F, u("data.bin")), substg(0x3701, 0x0102, big),
        Node("__properties_version1.0", props(b"\0" * 8, [(0x3705, 0x0003, struct.pack("<Q", 1))])),
    ])
    root = [
        substg(0x0037, 0x001F, u("Quarterly figures – draft")),
        substg(0x001A, 0x001F, u("IPM.Note")),
        substg(0x0C1A, 0x001F, u("José García")),
        substg(0x5D01, 0x001F, u("jose@example.org")),
        substg(0x0E04, 0x001F, u("Ann Doe")),
        substg(0x0E03, 0x001F, u("Bob Smith")),
        substg(0x1000, 0x001F, u("Hello Ann,\r\n\r\nThe figures are attached.\r\nJosé")),
        substg(0x007D, 0x001F, u(headers)),
        substg(0x1035, 0x001F, u("<msg-1@example.org>")),
        Node("__properties_version1.0", props(b"\0" * 8 + struct.pack("<IIII", 2, 2, 2, 2) + b"\0" * 8, [
            (0x0039, 0x0040, struct.pack("<Q", submit)),
            (0x0017, 0x0003, struct.pack("<Q", 2)),
            (0x0E08, 0x0003, struct.pack("<Q", 9000)),
        ])),
        recip1, recip2, attach1, attach2,
        Node("__nameid_version1.0", children=[]),
    ]
    write("message.msg", cfb(root))


# ----------------------------------------------------------------------------------------------
# Excel 97-2003 (BIFF8)

def rec(kind, data=b""):
    return struct.pack("<HH", kind, len(data)) + data


def xlstr16(s):
    return struct.pack("<HB", len(s), 0) + s.encode("latin-1")


def workbook(encrypted=False):
    strings = ["Name", "Amount", "Date", "Flag", "Formula", "Café", "Zoë Ünal", "Second sheet"]
    # SST: the 7th string ("Zoë Ünal") is split by a CONTINUE that switches to 16-bit characters.
    sst = struct.pack("<II", len(strings) + 2, len(strings))
    cont = b""
    for s in strings:
        if s == "Zoë Ünal":
            sst += struct.pack("<HB", len(s), 0) + s[:3].encode("latin-1")
            cont = b"\x01" + s[3:].encode("utf-16-le")
        elif cont:
            cont += struct.pack("<HB", len(s), 0) + s.encode("latin-1")
        else:
            sst += struct.pack("<HB", len(s), 0) + s.encode("latin-1")
    xfs = b""
    for i in range(18):
        ifmt = {16: 164, 17: 14}.get(i, 0)
        xfs += rec(0x00E0, struct.pack("<HHHBBBBIIH", 0, ifmt, 0xFFF5 if i < 15 else 1, 0x20, 0, 0, 0, 0, 0, 0x20C0))

    def bof(dt):
        return rec(0x0809, struct.pack("<HHHHII", 0x0600, dt, 0x0DBB, 0x07CC, 0, 0x0206))

    def boundsheet(pos, name):
        return rec(0x0085, struct.pack("<IBB", pos, 0, 0) + struct.pack("<BB", len(name), 0) + name.encode("latin-1"))

    def cell(r, c, xf):
        return struct.pack("<HHH", r, c, xf)

    def labelsst(r, c, i):
        return rec(0x00FD, cell(r, c, 15) + struct.pack("<I", i))

    def number(r, c, x, xf=15):
        return rec(0x0203, cell(r, c, xf) + struct.pack("<d", x))

    def rk_int(v):
        return (v << 2) | 2

    sheet1 = bof(0x0010) + rec(0x0200, struct.pack("<IIHHH", 0, 5, 0, 5, 0))
    sheet1 += b"".join(labelsst(0, c, i) for c, i in enumerate(range(5)))
    sheet1 += labelsst(1, 0, 5) + number(1, 1, 1234.5) + number(1, 2, 46327, 16)
    sheet1 += rec(0x0205, cell(1, 3, 15) + b"\x01\x00")
    sheet1 += rec(0x0006, cell(1, 4, 15) + struct.pack("<d", 2469.0) + struct.pack("<HI", 0, 0) + struct.pack("<H", 0))
    sheet1 += labelsst(2, 0, 6) + rec(0x027E, cell(2, 1, 15) + struct.pack("<I", rk_int(42)))
    sheet1 += rec(0x027E, cell(2, 2, 17) + struct.pack("<I", rk_int(46328)))
    sheet1 += rec(0x0205, cell(2, 3, 15) + b"\x07\x01")
    sheet1 += rec(0x0006, cell(2, 4, 15) + b"\x00\x00\x00\x00\x00\x00\xff\xff" + struct.pack("<HI", 0, 0) + struct.pack("<H", 0))
    sheet1 += rec(0x0207, xlstr16("cached text"))
    # MULRK: row 3, columns 1-2: 12.34 (x100) and 7.
    sheet1 += rec(0x00BD, struct.pack("<HH", 3, 1) + struct.pack("<HI", 15, (1234 << 2) | 3) + struct.pack("<HI", 15, rk_int(7)) + struct.pack("<H", 2))
    sheet1 += rec(0x0204, cell(3, 0, 15) + xlstr16("inline label"))
    sheet1 += rec(0x000A)
    sheet2 = bof(0x0010) + labelsst(0, 0, 7) + number(1, 0, 3.25) + rec(0x000A)

    def globals_(pos1, pos2):
        g = bof(0x0005)
        if encrypted:
            g += rec(0x002F, struct.pack("<HHH", 1, 1, 1) + b"\0" * 48)
        g += rec(0x0042, struct.pack("<H", 1200)) + rec(0x0022, struct.pack("<H", 0))
        g += rec(0x041E, struct.pack("<H", 164) + xlstr16("dd/mm/yyyy")) + xfs
        g += boundsheet(pos1, "Data") + boundsheet(pos2, "Notes")
        g += rec(0x00FC, sst) + rec(0x003C, cont) + rec(0x000A)
        return g
    g = globals_(0, 0)
    g = globals_(len(g), len(g) + len(sheet1))
    stream = g + sheet1 + sheet2
    stream = stream.ljust(4096, b"\0")
    write("encrypted.xls" if encrypted else "workbook.xls", cfb([Node("Workbook", stream)]))


# ----------------------------------------------------------------------------------------------
# Presentations

def zipped(name, files, compress=zipfile.ZIP_DEFLATED):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        for path, data in files:
            info = zipfile.ZipInfo(path, STAMP)
            info.compress_type = zipfile.ZIP_STORED if path == "mimetype" else compress
            z.writestr(info, data)
    write(name, buf.getvalue())


P = 'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"'
REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"


def sp(ph, paragraphs):
    phx = '<p:nvPr><p:ph type="%s"/></p:nvPr>' % ph if ph else "<p:nvPr/>"
    paras = "".join("<a:p><a:r><a:t>%s</a:t></a:r></a:p>" % p for p in paragraphs)
    return '<p:sp><p:nvSpPr><p:cNvPr id="2" name="s"/><p:cNvSpPr/>%s</p:nvSpPr><p:txBody><a:bodyPr/>%s</p:txBody></p:sp>' % (phx, paras)


def slide(*shapes, pic=None):
    pics = ('<p:pic><p:nvPicPr><p:cNvPr id="9" name="Picture"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr>'
            '<p:blipFill><a:blip r:embed="%s"/></p:blipFill></p:pic>' % pic) if pic else ""
    return '<?xml version="1.0" encoding="UTF-8"?><p:sld %s><p:cSld><p:spTree>%s%s</p:spTree></p:cSld></p:sld>' % (P, "".join(shapes), pics)


def pptx():
    files = [
        ("[Content_Types].xml", '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/></Types>'),
        ("_rels/.rels", '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="%s/officeDocument" Target="ppt/presentation.xml"/></Relationships>' % REL),
        ("ppt/presentation.xml", '<?xml version="1.0"?><p:presentation %s><p:sldIdLst><p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst></p:presentation>' % P),
        ("ppt/_rels/presentation.xml.rels", '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="%s/slide" Target="slides/slide1.xml"/><Relationship Id="rId3" Type="%s/slide" Target="slides/slide2.xml"/></Relationships>' % (REL, REL)),
        ("ppt/slides/slide2.xml", slide(sp("ctrTitle", ["Quarterly results"]), sp("subTitle", ["Example Research S.A."]))),
        ("ppt/slides/slide1.xml", slide(sp("title", ["Revenue &amp; costs"]), sp("body", ["Revenue up 12 %", "Costs flat"]), sp(None, ["Footnote box"]), pic="rId7")),
        ("ppt/slides/_rels/slide1.xml.rels", '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId7" Type="%s/image" Target="../media/image1.png"/><Relationship Id="rId8" Type="%s/notesSlide" Target="../notesSlides/notesSlide1.xml"/></Relationships>' % (REL, REL)),
        ("ppt/notesSlides/notesSlide1.xml", '<?xml version="1.0"?><p:notes %s><p:cSld><p:spTree>%s%s</p:spTree></p:cSld></p:notes>' % (P, sp("sldImg", []), sp("body", ["Mention the new contract."]))),
        ("ppt/media/image1.png", PNG_RED),
        ("docProps/core.xml", '<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/"><dc:title>Q3 review</dc:title><dc:creator>Ann Doe</dc:creator><dcterms:created>2026-10-30T08:00:00Z</dcterms:created></cp:coreProperties>'),
        ("docProps/app.xml", '<?xml version="1.0"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>Example Office</Application><Slides>2</Slides></Properties>'),
    ]
    zipped("deck.pptx", files)


def odp():
    ns = ('xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" '
          'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" '
          'xmlns:xlink="http://www.w3.org/1999/xlink"')
    content = ('<?xml version="1.0"?><office:document-content %s><office:body><office:presentation>'
               '<draw:page draw:name="Intro"><draw:frame presentation:class="title"><draw:text-box><text:p>Welcome</text:p></draw:text-box></draw:frame>'
               '<draw:frame presentation:class="outline"><draw:text-box><text:p>First <text:span>point</text:span></text:p><text:p>Second point</text:p></draw:text-box></draw:frame>'
               '<draw:frame><draw:image xlink:href="Pictures/blue.png"/></draw:frame>'
               '<presentation:notes><draw:frame><draw:text-box><text:p>Say hello.</text:p></draw:text-box></draw:frame></presentation:notes></draw:page>'
               '<draw:page draw:name="Closing"><draw:frame><draw:text-box><text:p>Thank you</text:p></draw:text-box></draw:frame></draw:page>'
               '</office:presentation></office:body></office:document-content>') % ns
    meta = ('<?xml version="1.0"?><office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/">'
            '<office:meta><dc:title>Kick-off</dc:title><meta:initial-creator>Bob Smith</meta:initial-creator><meta:creation-date>2026-10-01T09:00:00</meta:creation-date></office:meta></office:document-meta>')
    zipped("deck.odp", [("mimetype", "application/vnd.oasis.opendocument.presentation"), ("content.xml", content),
                        ("meta.xml", meta), ("Pictures/blue.png", PNG_BLUE)])


def ppt():
    def r(kind, data=b"", ver=0, inst=0):
        return struct.pack("<HHI", ver | (inst << 4), kind, len(data)) + data

    def chars(s):
        return r(0x0FA0, s.encode("utf-16-le"))

    def bytes_(s):
        return r(0x0FA8, s.encode("cp1252"))

    def header(t):
        return r(0x0F9F, struct.pack("<I", t))
    persist = r(0x03F3, b"\0" * 20)
    slides = r(0x0FF0, persist + header(0) + chars("Annual review") + header(1) + bytes_("Revenue grew\rCosts fell")
               + persist + header(0) + bytes_("Next steps") + header(1) + chars("Hire two people"), ver=0xF, inst=0)
    notes = r(0x0FF0, persist + header(2) + bytes_("Thank the team"), ver=0xF, inst=2)
    doc = r(0x03E8, r(0x03E9, b"\0" * 40) + slides + notes, ver=0xF)
    textbox = r(0xF00D, bytes_("Draft – confidential"), ver=0xF)
    slide1 = r(0x03EE, r(0x03EF, b"\0" * 24) + r(0x040C, r(0xF002, r(0xF004, textbox, ver=0xF), ver=0xF), ver=0xF), ver=0xF)
    slide2 = r(0x03EE, r(0x03EF, b"\0" * 24), ver=0xF)
    master = r(0x03F8, bytes_("Click to edit Master title style"), ver=0xF)
    stream = r(0x0FF5, b"\0" * 20) + doc + master + slide1 + slide2
    write("deck.ppt", cfb([Node("PowerPoint Document", stream), Node("Current User", b"\0" * 32)]))


# ----------------------------------------------------------------------------------------------
# DER (certificates and CMS) for the signed containers

def der(tag, body):
    n = len(body)
    if n < 0x80:
        length = bytes([n])
    elif n < 0x100:
        length = b"\x81" + bytes([n])
    else:
        length = b"\x82" + struct.pack(">H", n)
    return bytes([tag]) + length + body


def seq(*parts):
    return der(0x30, b"".join(parts))


def oid(dotted):
    nums = [int(x) for x in dotted.split(".")]
    out = bytes([nums[0] * 40 + nums[1]])
    for n in nums[2:]:
        chunk = [n & 0x7F]
        n >>= 7
        while n:
            chunk.append(0x80 | (n & 0x7F))
            n >>= 7
        out += bytes(reversed(chunk))
    return der(0x06, out)


def name(*pairs):
    return seq(*[der(0x31, seq(oid(o), der(0x0C, v.encode()))) for o, v in pairs])


ISSUER = name(("2.5.4.6", "LU"), ("2.5.4.10", "Example Trust Services"), ("2.5.4.3", "Example Qualified CA"))
SUBJECT = name(("2.5.4.6", "LU"), ("2.5.4.42", "Ann"), ("2.5.4.4", "Example"), ("2.5.4.3", "Ann Example"))
SERIAL = der(0x02, b"\x01\x23\x45")


def certificate():
    alg = seq(oid("1.2.840.113549.1.1.11"), der(0x05, b""))
    tbs = seq(der(0xA0, der(0x02, b"\x02")), SERIAL, alg, ISSUER,
              seq(der(0x17, b"260101000000Z"), der(0x17, b"281231235959Z")), SUBJECT,
              seq(seq(oid("1.2.840.113549.1.1.1"), der(0x05, b"")), der(0x03, b"\x00" + b"\x30\x03\x02\x01\x03")))
    return seq(tbs, alg, der(0x03, b"\x00" + b"\x5a" * 32))


def cms():
    cert = certificate()
    signing_time = seq(oid("1.2.840.113549.1.9.5"), der(0x31, der(0x17, b"261101090000Z")))
    signer = seq(der(0x02, b"\x01"), seq(ISSUER, SERIAL), seq(oid("2.16.840.1.101.3.4.2.1")),
                 der(0xA0, signing_time), seq(oid("1.2.840.113549.1.1.11")), der(0x04, b"\x77" * 64))
    signed = seq(der(0x02, b"\x01"), der(0x31, seq(oid("2.16.840.1.101.3.4.2.1"))),
                 seq(oid("1.2.840.113549.1.7.1")), der(0xA0, cert), der(0x31, signer))
    return seq(oid("1.2.840.113549.1.7.2"), der(0xA0, signed))


# ----------------------------------------------------------------------------------------------
# ASiC-E

def asice(name_, extra=None):
    cert = base64.b64encode(certificate()).decode()
    sig = ('<?xml version="1.0" encoding="UTF-8"?>'
           '<asic:XAdESSignatures xmlns:asic="http://uri.etsi.org/02918/v1.2.1#" xmlns:ds="http://www.w3.org/2000/09/xmldsig#" xmlns:xades="http://uri.etsi.org/01903/v1.3.2#">'
           '<ds:Signature Id="S0"><ds:SignedInfo><ds:CanonicalizationMethod Algorithm="http://www.w3.org/2006/12/xml-c14n11"/>'
           '<ds:SignatureMethod Algorithm="http://www.w3.org/2001/04/xmldsig-more#rsa-sha256"/>'
           '<ds:Reference Id="r1" URI="invoice.pdf"><ds:DigestMethod Algorithm="http://www.w3.org/2001/04/xmlenc#sha256"/><ds:DigestValue>n4bQgYhMfWWaL+qgxVrQFaO/TxsrC4Is0V1sFbDwCgg=</ds:DigestValue></ds:Reference>'
           '<ds:Reference Id="r2" URI="data%%20file.xml"><ds:DigestMethod Algorithm="http://www.w3.org/2001/04/xmlenc#sha512"/><ds:DigestValue>AAAA</ds:DigestValue></ds:Reference>'
           '<ds:Reference Type="http://uri.etsi.org/01903#SignedProperties" URI="#S0-SignedProperties"><ds:DigestMethod Algorithm="http://www.w3.org/2001/04/xmlenc#sha256"/><ds:DigestValue>BBBB</ds:DigestValue></ds:Reference>'
           '</ds:SignedInfo><ds:SignatureValue>c2lnbmF0dXJl</ds:SignatureValue>'
           '<ds:KeyInfo><ds:X509Data><ds:X509Certificate>%s</ds:X509Certificate></ds:X509Data></ds:KeyInfo>'
           '<ds:Object><xades:QualifyingProperties Target="#S0"><xades:SignedProperties Id="S0-SignedProperties"><xades:SignedSignatureProperties>'
           '<xades:SigningTime>2026-11-01T09:00:00Z</xades:SigningTime></xades:SignedSignatureProperties></xades:SignedProperties></xades:QualifyingProperties></ds:Object>'
           '</ds:Signature></asic:XAdESSignatures>') % cert
    manifest = ('<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">'
                '<manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.etsi.asic-e+zip"/>'
                '<manifest:file-entry manifest:full-path="invoice.pdf" manifest:media-type="application/pdf"/>'
                '<manifest:file-entry manifest:full-path="data file.xml" manifest:media-type="application/xml"/></manifest:manifest>')
    files = [("mimetype", "application/vnd.etsi.asic-e+zip"), ("invoice.pdf", b"%PDF-1.4\n%%EOF\n"),
             ("data file.xml", "<data>42</data>"), ("META-INF/manifest.xml", manifest),
             ("META-INF/signatures0.xml", sig)]
    if extra:
        files.append(extra)
    zipped(name_, files)


# ----------------------------------------------------------------------------------------------
# PDF

class PdfWriter:
    def __init__(self):
        self.objects = []

    def add(self, body):
        self.objects.append(body if isinstance(body, bytes) else body.encode("latin-1"))
        return len(self.objects)

    def stream(self, dict_body, data):
        return self.add(b"<< " + dict_body.encode("latin-1") + b" /Length %d >>\nstream\n" % len(data) + data + b"\nendstream")

    def build(self, root, info):
        out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
        offsets = []
        for i, body in enumerate(self.objects, 1):
            offsets.append(len(out))
            out += b"%d 0 obj\n" % i + body + b"\nendobj\n"
        xref = len(out)
        out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(self.objects) + 1)
        for o in offsets:
            out += b"%010d 00000 n \n" % o
        out += b"trailer\n<< /Size %d /Root %d 0 R /Info %d 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(self.objects) + 1, root, info, xref)
        return bytes(out)


CII = """<?xml version="1.0" encoding="UTF-8"?>
<rsm:CrossIndustryInvoice xmlns:rsm="urn:un:unece:uncefact:data:standard:CrossIndustryInvoice:100" xmlns:ram="urn:un:unece:uncefact:data:standard:ReusableAggregateBusinessInformationEntity:100" xmlns:udt="urn:un:unece:uncefact:data:standard:UnqualifiedDataType:100">
  <rsm:ExchangedDocumentContext><ram:GuidelineSpecifiedDocumentContextParameter><ram:ID>urn:cen.eu:en16931:2017</ram:ID></ram:GuidelineSpecifiedDocumentContextParameter></rsm:ExchangedDocumentContext>
  <rsm:ExchangedDocument><ram:ID>INV-2026-0042</ram:ID><ram:TypeCode>380</ram:TypeCode><ram:IssueDateTime><udt:DateTimeString format="102">20261101</udt:DateTimeString></ram:IssueDateTime></rsm:ExchangedDocument>
  <rsm:SupplyChainTradeTransaction>
    <ram:IncludedSupplyChainTradeLineItem><ram:AssociatedDocumentLineDocument><ram:LineID>1</ram:LineID></ram:AssociatedDocumentLineDocument><ram:SpecifiedTradeProduct><ram:Name>Consulting day</ram:Name></ram:SpecifiedTradeProduct>
      <ram:SpecifiedLineTradeAgreement><ram:NetPriceProductTradePrice><ram:ChargeAmount>800.00</ram:ChargeAmount></ram:NetPriceProductTradePrice></ram:SpecifiedLineTradeAgreement>
      <ram:SpecifiedLineTradeDelivery><ram:BilledQuantity unitCode="DAY">2</ram:BilledQuantity></ram:SpecifiedLineTradeDelivery>
      <ram:SpecifiedLineTradeSettlement><ram:ApplicableTradeTax><ram:CategoryCode>S</ram:CategoryCode><ram:RateApplicablePercent>17</ram:RateApplicablePercent></ram:ApplicableTradeTax><ram:SpecifiedTradeSettlementLineMonetarySummation><ram:LineTotalAmount>1600.00</ram:LineTotalAmount></ram:SpecifiedTradeSettlementLineMonetarySummation></ram:SpecifiedLineTradeSettlement></ram:IncludedSupplyChainTradeLineItem>
    <ram:IncludedSupplyChainTradeLineItem><ram:AssociatedDocumentLineDocument><ram:LineID>2</ram:LineID></ram:AssociatedDocumentLineDocument><ram:SpecifiedTradeProduct><ram:Name>Travel</ram:Name></ram:SpecifiedTradeProduct>
      <ram:SpecifiedLineTradeAgreement><ram:NetPriceProductTradePrice><ram:ChargeAmount>150.00</ram:ChargeAmount></ram:NetPriceProductTradePrice></ram:SpecifiedLineTradeAgreement>
      <ram:SpecifiedLineTradeDelivery><ram:BilledQuantity unitCode="C62">1</ram:BilledQuantity></ram:SpecifiedLineTradeDelivery>
      <ram:SpecifiedLineTradeSettlement><ram:ApplicableTradeTax><ram:CategoryCode>S</ram:CategoryCode><ram:RateApplicablePercent>17</ram:RateApplicablePercent></ram:ApplicableTradeTax><ram:SpecifiedTradeSettlementLineMonetarySummation><ram:LineTotalAmount>150.00</ram:LineTotalAmount></ram:SpecifiedTradeSettlementLineMonetarySummation></ram:SpecifiedLineTradeSettlement></ram:IncludedSupplyChainTradeLineItem>
    <ram:ApplicableHeaderTradeAgreement><ram:BuyerReference>PO-77</ram:BuyerReference>
      <ram:SellerTradeParty><ram:Name>Example Research S.A.</ram:Name><ram:PostalTradeAddress><ram:CityName>Luxembourg</ram:CityName></ram:PostalTradeAddress><ram:SpecifiedTaxRegistration><ram:ID schemeID="VA">LU12345678</ram:ID></ram:SpecifiedTaxRegistration></ram:SellerTradeParty>
      <ram:BuyerTradeParty><ram:Name>Client &amp; Partners GmbH</ram:Name></ram:BuyerTradeParty></ram:ApplicableHeaderTradeAgreement>
    <ram:ApplicableHeaderTradeDelivery/>
    <ram:ApplicableHeaderTradeSettlement><ram:InvoiceCurrencyCode>EUR</ram:InvoiceCurrencyCode>
      <ram:SpecifiedTradeSettlementPaymentMeans><ram:TypeCode>58</ram:TypeCode><ram:PayeePartyCreditorFinancialAccount><ram:IBANID>LU280019400644750000</ram:IBANID></ram:PayeePartyCreditorFinancialAccount></ram:SpecifiedTradeSettlementPaymentMeans>
      <ram:ApplicableTradeTax><ram:CalculatedAmount>297.50</ram:CalculatedAmount><ram:TypeCode>VAT</ram:TypeCode><ram:BasisAmount>1750.00</ram:BasisAmount><ram:CategoryCode>S</ram:CategoryCode><ram:RateApplicablePercent>17</ram:RateApplicablePercent></ram:ApplicableTradeTax>
      <ram:SpecifiedTradePaymentTerms><ram:DueDateDateTime><udt:DateTimeString format="102">20261201</udt:DateTimeString></ram:DueDateDateTime></ram:SpecifiedTradePaymentTerms>
      <ram:SpecifiedTradeSettlementHeaderMonetarySummation><ram:LineTotalAmount>1750.00</ram:LineTotalAmount><ram:TaxBasisTotalAmount>1750.00</ram:TaxBasisTotalAmount><ram:TaxTotalAmount currencyID="EUR">297.50</ram:TaxTotalAmount><ram:GrandTotalAmount>2047.50</ram:GrandTotalAmount><ram:DuePayableAmount>2047.50</ram:DuePayableAmount></ram:SpecifiedTradeSettlementHeaderMonetarySummation>
    </ram:ApplicableHeaderTradeSettlement>
  </rsm:SupplyChainTradeTransaction>
</rsm:CrossIndustryInvoice>
"""

XMP = """<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:pdf="http://ns.adobe.com/pdf/1.3/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/" xmlns:fx="urn:factur-x:pdfa:CrossIndustryDocument:invoice:1p0#" pdf:Producer="Example PDF Writer 2.0">
<dc:title><rdf:Alt><rdf:li xml:lang="x-default">Invoice INV-2026-0042</rdf:li></rdf:Alt></dc:title>
<xmp:CreateDate>2026-11-01T10:00:00+01:00</xmp:CreateDate>
<pdfaid:part>3</pdfaid:part><pdfaid:conformance>B</pdfaid:conformance>
<fx:DocumentType>INVOICE</fx:DocumentType><fx:DocumentFileName>factur-x.xml</fx:DocumentFileName><fx:Version>1.0</fx:Version><fx:ConformanceLevel>EN 16931</fx:ConformanceLevel>
</rdf:Description></rdf:RDF></x:xmpmeta>
<?xpacket end="w"?>"""


def facturx():
    w = PdfWriter()
    xml = CII.encode()
    packed = zlib.compress(xml)
    ef = w.stream("/Type /EmbeddedFile /Subtype /text#2Fxml /Filter /FlateDecode /Params << /Size %d /ModDate (D:20261101100000+01'00') >>" % len(xml), packed)
    spec = w.add("<< /Type /Filespec /F (factur-x.xml) /UF (factur-x.xml) /Desc (Factur-X invoice) /AFRelationship /Data /EF << /F %d 0 R /UF %d 0 R >> >>" % (ef, ef))
    note = w.stream("/Type /EmbeddedFile /Subtype /text#2Fplain", b"Delivery note 17")
    note_spec = w.add("<< /Type /Filespec /F (note.txt) /EF << /F %d 0 R >> >>" % note)
    annot = w.add("<< /Type /Annot /Subtype /FileAttachment /Rect [10 10 30 30] /FS %d 0 R >>" % note_spec)
    content = w.stream("", b"BT /F1 12 Tf 72 720 Td (Invoice INV-2026-0042) Tj ET")
    font = w.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    pages_index = len(w.objects) + 2
    page = w.add("<< /Type /Page /Parent %d 0 R /MediaBox [0 0 595 842] /Contents %d 0 R /Resources << /Font << /F1 %d 0 R >> >> /Annots [%d 0 R] >>" % (pages_index, content, font, annot))
    pages = w.add("<< /Type /Pages /Kids [%d 0 R] /Count 1 >>" % page)
    assert pages == pages_index
    meta = w.stream("/Type /Metadata /Subtype /XML", XMP.encode())
    root = w.add("<< /Type /Catalog /Pages %d 0 R /Metadata %d 0 R /Names << /EmbeddedFiles << /Names [(factur-x.xml) %d 0 R] >> >> /AF [%d 0 R] >>" % (pages, meta, spec, spec))
    info = w.add("<< /Title (Invoice INV-2026-0042) /Author (Example Research S.A.) /Producer (Example PDF Writer 2.0) /CreationDate (D:20261101100000+01'00') >>")
    write("facturx.pdf", w.build(root, info))


def signed():
    w = PdfWriter()
    content = w.stream("", b"BT /F1 12 Tf 72 720 Td (Signed contract) Tj ET")
    font = w.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    hexlen = 4096
    placeholder = b"[0 0000000000 0000000000 0000000000]"
    sig = w.add(b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /ETSI.CAdES.detached /Name (Ann Example) /Reason (Approval) "
                b"/Location (Luxembourg) /M (D:20261101100000+01'00') /ByteRange " + placeholder + b" /Contents <" + b"0" * hexlen + b"> >>")
    pages_index = len(w.objects) + 3
    field = w.add("<< /FT /Sig /T (Signature1) /V %d 0 R /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /F 132 /P %d 0 R >>" % (sig, pages_index - 1))
    page = w.add("<< /Type /Page /Parent %d 0 R /MediaBox [0 0 595 842] /Contents %d 0 R /Resources << /Font << /F1 %d 0 R >> >> /Annots [%d 0 R] >>" % (pages_index, content, font, field))
    pages = w.add("<< /Type /Pages /Kids [%d 0 R] /Count 1 >>" % page)
    assert pages == pages_index
    root = w.add("<< /Type /Catalog /Pages %d 0 R /AcroForm << /Fields [%d 0 R] /SigFlags 3 >> >>" % (pages, field))
    info = w.add("<< /Title (Contract) /Producer (Example Signer) >>")
    data = bytearray(w.build(root, info))
    at = data.index(b"/Contents <") + len(b"/Contents ")
    hole_end = at + hexlen + 2
    br = b"[0 %010d %010d %010d]" % (at, hole_end, len(data) - hole_end)
    assert len(br) == len(placeholder)
    p = data.index(placeholder)
    data[p:p + len(placeholder)] = br
    blob = cms().hex().encode()
    data[at + 1:at + 1 + len(blob)] = blob
    write("signed.pdf", bytes(data))


# ----------------------------------------------------------------------------------------------
# Fixed-width records and bordereaux

def fixed_width():
    layout = """# Layout of payments.dat (fixed-width, one record per line)
name = "Example payment file v2"

[[record]]
name = "Header"
match_start = 1
match = "H"
[[record.field]]
name = "Record type"
start = 1
length = 1
[[record.field]]
name = "Created"
start = 2
length = 8
type = "date"
pattern = "YYYYMMDD"
[[record.field]]
name = "Sender"
start = 10
length = 20

[[record]]
name = "Payment"
match_start = 1
match = "D"
[[record.field]]
name = "Account"
start = 2
length = 10
[[record.field]]
name = "Amount"
start = 12
length = 10
type = "decimal"
decimals = 2
[[record.field]]
name = "Value date"
start = 22
length = 8
type = "date"
[[record.field]]
name = "Payee name"
start = 30
length = 20

[[record]]
name = "Trailer"
match = "T"
[[record.field]]
name = "Count"
start = 2
length = 6
type = "integer"
"""
    data = ("H20261101Example Research SA \n"
            "D00123456780000125050202611 2Café Müller         \n"
            "D00987654320000009999202611 3Client & Partners   \n"
            "D0055555555000012A456202611 4Broken amount       \n"
            "X this line has no record type\n"
            "T000003\n")
    data = data.replace("202611 2", "20261102").replace("202611 3", "20261103").replace("202611 4", "20261199")
    write("payments.dat", data)
    write("payments.dat.layout.toml", layout)


def bordereaux():
    write("premium-bordereau.csv",
          "Premium bordereau November 2026,,,,,,,,\n"
          "UMR,Policy Ref,Insured Name,Inception Date,Expiry Date,Currency,Gross Premium,Commission,Net Premium\n"
          "B0999XYZ2026,POL-001,Café Müller SARL,01/11/2026,31/10/2027,EUR,\"1,250.00\",250.00,1000.00\n"
          "B0999XYZ2026,POL-002,Client & Partners GmbH,15/11/2026,14/11/2027,EUR,980.50,196.10,784.40\n"
          "B0999XYZ2026,POL-003,Example Ltd,2026-12-01,2027-11-30,GBP,500,100,400\n"
          "B0999XYZ2026,POL-004,Late Entry Co,32/13/2026,2027-11-30,GBP,n/a,0,0\n")
    # Claims bordereau as XLSX (shared strings, a date style).
    strings = ["Claim Number", "Policy Number", "Insured", "Date of Loss", "Currency", "Paid", "Outstanding Reserve", "Status",
               "CLM-1", "POL-001", "Café Müller SARL", "EUR", "Open", "CLM-2", "POL-003", "Example Ltd", "GBP", "Closed"]
    sst = ('<?xml version="1.0"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="%d" uniqueCount="%d">' % (len(strings), len(strings))
           + "".join("<si><t>%s</t></si>" % s for s in strings) + "</sst>")

    def c(ref, v, t=None, s=None):
        attrs = ' r="%s"' % ref + (' t="%s"' % t if t else "") + (' s="%s"' % s if s else "")
        return "<c%s><v>%s</v></c>" % (attrs, v)
    rows = [
        [c("A1", 0, "s"), c("B1", 1, "s"), c("C1", 2, "s"), c("D1", 3, "s"), c("E1", 4, "s"), c("F1", 5, "s"), c("G1", 6, "s"), c("H1", 7, "s")],
        [c("A2", 8, "s"), c("B2", 9, "s"), c("C2", 10, "s"), c("D2", 46330, s=1), c("E2", 11, "s"), c("F2", "1200.5"), c("G2", "3000"), c("H2", 12, "s")],
        [c("A3", 13, "s"), c("B3", 14, "s"), c("C3", 15, "s"), c("D3", 46340, s=1), c("E3", 16, "s"), c("F3", "800"), c("G3", "0"), c("H3", 17, "s")],
    ]
    sheet = ('<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>'
             + "".join('<row r="%d">%s</row>' % (i + 1, "".join(r)) for i, r in enumerate(rows)) + "</sheetData></worksheet>")
    styles = ('<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
              '<cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14"/></cellXfs></styleSheet>')
    wb = ('<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="%s">'
          '<sheets><sheet name="Claims" sheetId="1" r:id="rId1"/></sheets></workbook>' % REL)
    wbrels = ('<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
              '<Relationship Id="rId1" Type="%s/worksheet" Target="worksheets/sheet1.xml"/>'
              '<Relationship Id="rId2" Type="%s/sharedStrings" Target="sharedStrings.xml"/>'
              '<Relationship Id="rId3" Type="%s/styles" Target="styles.xml"/></Relationships>' % (REL, REL, REL))
    ct = ('<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/>'
          '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>')
    zipped("claims-bordereau.xlsx", [
        ("[Content_Types].xml", ct),
        ("xl/sharedStrings.xml", sst), ("xl/worksheets/sheet1.xml", sheet), ("xl/workbook.xml", wb),
        ("xl/_rels/workbook.xml.rels", wbrels), ("xl/styles.xml", styles),
        ("_rels/.rels", '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="%s/officeDocument" Target="xl/workbook.xml"/></Relationships>' % REL),
    ])
    write("mapped.csv", "Ref;Who;Start;GWP Orig;Ccy\nA-1;Ann Doe;2026-11-01;100,50;EUR\nA-2;Bob Smith;2026-11-02;99,50;EUR\n")
    write("mapped.bordereau.toml", '# Column meanings for mapped.csv\n[columns]\n"Ref" = "policy_number"\n"Who" = "insured"\n"Start" = "inception"\n"GWP Orig" = "gross_premium"\n"Ccy" = "currency"\n')


def main():
    eml()
    mbox()
    msg()
    workbook()
    workbook(encrypted=True)
    pptx()
    odp()
    ppt()
    asice("container.asice")
    asice("unsigned-entry.asice", extra=("readme.txt", "added after signing"))
    facturx()
    signed()
    fixed_width()
    bordereaux()


if __name__ == "__main__":
    sys.exit(main())
