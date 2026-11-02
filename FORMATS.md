# Formats

Every file opens: a format below gets the views listed; any other file gets **Summary** (what it is,
its size, the facts its header gives: executables, images, audio, video, fonts, disk images and
more), **Strings** (readable text in the bytes, filterable, each opening the bytes at its offset)
and **Hex**, or **Source** when it is text. Every binary format below also gets Strings. Text over 16 MiB is
read a page at a time, read-only, with find and go to line, whatever its size and whether it is
UTF-8, UTF-16 or Windows-1252.

## Edited as text

| Format | Views | Notes |
|---|---|---|
| Markdown | Preview, Source, Outline | Live preview beside the editor; GitHub tables, task lists, footnotes, math; local images only. |
| TeX, LaTeX | Source, Outline | Outline of sections, environments, labels, citations and included files. Nothing is compiled. |
| BibTeX | Table, Source | Entries as a table; duplicate keys and missing required fields reported. |
| JSON, JSON Lines | Tree, Source | Strict JSON; comments and trailing commas accepted with a warning; duplicate keys and integers beyond 2^53 reported. Formatted view for minified files. |
| XML (SVG, XSD, XSLT, plist, …) | Tree, Source | Namespace-aware; no DTD, no entity expansion, nothing fetched. Formatted view for minified files. |
| YAML | Tree, Source | Aliases are never expanded. |
| TOML, INI, `.properties`, `.env` | Tree, Source | Tables kept in file order; redefinitions and duplicates reported. Lock files (`Cargo.lock`, `poetry.lock`, `uv.lock`, `flake.lock`, `composer.lock`, …) and tool settings (`.editorconfig`, `.gitconfig`, `.npmrc`) recognized by name. |
| CSV, TSV | Table, Source | Delimiter and header detected; sort and filter; cells open in the text. |
| Source code (about 60 languages) | Source, Outline | Highlighting by extension, file name, modeline or shebang; outline of functions, types and other definitions for 31 languages. |
| Plain text, logs | Source | |

## Read-only

Every table, text or binary, can be sorted by a column and filtered; a binary file over 100,000
rows is sorted and filtered within its first 100,000 rows, and the window says so.

| Format | Views | Notes |
|---|---|---|
| MessagePack, Protobuf (`.pb`, `.binpb`) | Tree, Source, Summary, Strings, Hex | Decoded and shown as JSON, read-only. Protobuf has no schema: keys are field numbers, nested messages, text and bytes are guessed. |
| Jupyter notebook | Cells, Tree, Source | Outputs as saved; HTML and JavaScript shown as source; nothing is executed. |
| NumPy `.npy` | Table, Summary, Hex | Object arrays (pickled Python) are refused. |
| PNG, JPEG, GIF, WebP, BMP, ICO | Preview, Summary, Hex | Up to 64 MiB. |
| TIFF, BigTIFF | Preview, Summary, Strings, Hex | First image decoded in Rust (strips or tiles; LZW, Deflate, PackBits, JPEG, fax; grey, RGB, CMYK, YCbCr, palette, 1 to 64 bits, planes) and reduced to 8192 pixels a side. GeoTIFF opens on its Overview. |
| Photoshop (`.psd`, `.psb`), HEIF/HEIC, AVIF | Summary, Strings, Hex | Size, colour mode and depth from the header; the picture is not decoded. |
| SQLite database | Table, Summary, Hex | Tables and views with their declared types; read page by page, no SQL is run. |
| Excel workbook (`.xlsx`, `.xlsm`) | Table, Summary, Hex | One table per sheet, values as Excel displays them; formulas are not evaluated, macros never run. |
| Parquet, Arrow IPC, Feather | Table, Summary, Hex | Read row group by row group, or batch by batch; nested values as text. |
| Avro, ORC | Table, Summary, Hex | Read block by block or stripe by stripe; logical types (dates, decimals, UUIDs) shown as values. |
| HDF5, NetCDF-4, NumPy `.npz` | Table, Summary, Hex | One table per dataset or array, read without the HDF5 C library; pickled arrays are refused. |
| ZIP, tar, gzip, Zstandard, xz, bzip2 (and compressed tar) | Contents, Summary, Hex | Listed without extracting; an entry opens in the same window from a private copy. Unsafe paths and likely decompression bombs are flagged. |
| PDF | Preview, Summary, Hex | Pages drawn by a Rust renderer as they scroll into sight; no JavaScript, forms or links run. |
| Word (`.docx`), OpenDocument text (`.odt`) | Preview, Summary, Hex | Headings, lists, tables, notes and images as a page built by the reader; macros never run. |

## Industry standards

A file in a standard below opens on **Overview**, on top of its views above: the standard and its
version, what was checked and what was not (schemas are never claimed), the facts, tables, outline
and notes, and a picture where one helps (map, 3D wireframe, drawing, signals, timeline). Every
item links to its place in the file. Nothing is validated online and nothing is fetched.

| Family | Standards |
|---|---|
| Banking and payments | ISO 20022 (pain, pacs, camt), SWIFT MT, MT940/942/950, BAI2, FIX, OFX, QIF |
| Accounting and tax | UBL, UN/CEFACT CII, Factur-X/ZUGFeRD, XBRL and Inline XBRL, SAF-T (PT, OECD, JPK), French FEC, ACORD, fixed-width records, insurance bordereaux |
| EDI and logistics | UN/EDIFACT, ANSI X12, HL7 v2, Cargo-IMP, GS1 XML, IATA Cargo-XML |
| Clinical | FHIR (JSON, XML), HL7 CDA, DICOM |
| Science | EDF/BDF, BAM, CRAM, FITS, MATLAB `.mat`, VCF, FASTQ, FASTA |
| Geography | GeoJSON, KML/KMZ, GPX, Shapefile, GeoPackage, MBTiles, GeoTIFF, LAS/LAZ |
| Engineering | STL, OBJ, glTF/GLB, IFC, STEP, IGES, DXF, G-code, OPC UA NodeSet, AUTOSAR ARXML |
| Research and law | SDMX (XML, JSON, CSV), DDI, Akoma Ntoso, Jupyter |
| Documents | E-mail (EML, mbox), Outlook MSG, legacy Excel XLS, presentations (PPTX, ODP, PPT), ASiC-E containers, PDF signatures and attachments |
| Security | X.509 certificates, PEM, DER, CMS/PKCS#7, PKCS#12, JWT, XML signatures and XAdES (signatures are described, not verified) |
| Software and networks | SARIF, CycloneDX, SPDX, HAR, WebAssembly, Java class, ELF, PE, pcap/pcapng |

## Copying out

Copying is always an explicit action and only writes to the clipboard. In a table, **Copy as**
puts the rows shown (in their order and filter, up to 10,000) on the clipboard as CSV, TSV, a
Markdown table or JSON; Ctrl/Cmd+C copies the selected cell. In a tree, Ctrl/Cmd+C copies the
selected node exactly as written in the file.
