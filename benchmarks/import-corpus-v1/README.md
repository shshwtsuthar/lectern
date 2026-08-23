# Lectern import benchmark corpus v1

This recipe prepares a local 10,000-file workload for exploratory EPUB and PDF import
measurements. It creates 7,000 EPUB files and 3,000 PDF files from 48 byte-pinned source
publications, with 100 files per shard and a modest heavy-tailed size distribution.

The expanded corpus contains repeated byte-identical copies of 36 EPUB and 12 PDF seeds. It is
useful for exercising discovery, parsing, copying, database writes, and a range of publication
sizes and structures. It is not evidence about a collection of 10,000 unique publications, and
results should describe it as a synthetic import workload rather than a representative catalog.

## Generate the corpus

Run from the repository root on Linux:

```bash
benchmarks/import-corpus-v1/prepare.sh --check-manifest
benchmarks/import-corpus-v1/prepare.sh
```

Set `LECTERN_CORPUS_JOBS` to change the default four concurrent downloads and copies:

```bash
LECTERN_CORPUS_JOBS=8 benchmarks/import-corpus-v1/prepare.sh
```

The script requires Bash 4 or newer, GNU coreutils/findutils, `curl`, `qpdf`, and `unzip`. It
writes only beneath the ignored directory `target/benchmarks/import-corpus-v1/` and is safe to
rerun when existing files match the manifest. It refuses to replace a mismatching seed or corpus
file; inspect and remove obsolete local data explicitly before rebuilding a different recipe.

Expected output:

- `corpus/`: 10,000 ordinary files with distinct inodes, split 70% EPUB and 30% PDF.
- `generation-plan.tsv`: deterministic mapping from each destination to a pinned seed.
- `corpus-manifest.json`: stable source, plan, and corpus fingerprints for benchmark results.
- Logical corpus size: 9,742,451,988 bytes (about 9.073 GiB).

Copies use `cp --reflink=never`, not symlinks, hard links, or copy-on-write reflinks. Before any
expansion, the script rejects a projected corpus over the 15 GiB target, has a separate 20 GiB
hard abort, and requires enough free space to retain a 40 GiB reserve after the projected copy.
The reserve intentionally leaves ample room for benchmark databases, covers, results, and normal
development on the available disk.

## Sources and rights

[`sources.tsv`](sources.tsv) is the sole source allowlist. It records each download URL, source
revision, byte length, SHA-256 digest, rights basis, rights URL, embedded or declared rights text,
and whether the item deserves additional review. Downloaded publications and expanded copies are
never repository content and must not be committed or redistributed as part of Lectern.

The EPUB seeds are official assets from the IDPF/W3C EPUB 3 Samples release `20230704`, whose tag
resolves to commit `46d7e07e1b39b2d0a0245ececaf896edcd9de4b2`. The upstream project states that
CC BY-SA 3.0 is the default only when its sample table does not specify different terms. Several
publications carry different or potentially conflicting embedded rights statements. In particular,
`accessible_epub_3.epub` and the two `indexing-for-eds-and-auths` samples are marked for review.

- Release: <https://github.com/IDPF/epub3-samples/releases/tag/20230704>
- Sample table and source-specific licensing notes:
  <https://idpf.github.io/epub3-samples/30/samples.html>
- Repository license statement: <https://github.com/IDPF/epub3-samples#licensing>

The PDF seeds are unmodified NIST FIPS, Special Publication, and Interagency Report documents.
NIST's technical-publication policy explains that employee-authored works are not protected by
copyright in the United States, reserves foreign rights while granting broad reprint permission,
requests attribution, and cautions that third-party-authored material can have separate rights.

- NIST policy:
  <https://www.nist.gov/open/copyright-fair-use-and-licensing-statements-srd-data-software-and-technical-series-publications>

The manifest preserves upstream statements for provenance; it is not a legal determination. Review
the current source-specific terms before any use beyond local benchmark preparation.

## Reproducibility boundary

The source manifest, generation plan, byte counts, and checksums define this corpus. File mtimes,
download time, filesystem allocation, and absolute paths do not. Benchmark result JSON should carry
the fingerprints from `corpus-manifest.json` so runs can be compared without committing corpus
bytes or machine-specific preparation logs. Updating any source, hash, mix, or disk guard requires
a reviewed recipe change rather than silently accepting new remote content.
